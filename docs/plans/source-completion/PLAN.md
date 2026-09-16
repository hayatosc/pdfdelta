# Connect source proofs to complete PDF comparison

Status: strict completion remains unmet; separately versioned observation completion approved for implementation. Created 2026-09-16.

Equal-candidate diagnostics found six controller/processor candidates blocked by
padded endpoint admission, and 50 Schedule C candidates blocked during native
preparation. Equal padded ranges now invoke the existing full-population SourceCut
validator, preserving explicit boundary remainders and all native/body checks.
The final same-six-pair pilot remains identical, with no new source or completion.
Workspace 2,509 passed (two ignored); final affected tests 89/89, generated 48/48,
Clippy and formatting pass. Evidence:
`benchmark/realworld/remaining/source-completion/padded-equal-domain.md`,
`equal-candidate-denials.json` and `padded-equal-domain-v2-pilot.json`.
Remaining native preparation/body failures require direct evidence; keep the goal active.

Equal native intervals now reach source validation without requiring a changed
review or selecting repeated occurrence edges. A repeated-paragraph fixture gains
complete source coverage while search uncertainty remains; missing or inconsistent
native evidence is rejected. Workspace: 2,508 passed, two ignored; focused 88/88,
generated 48/48, Clippy and formatting pass. The six-pair natural pilot adds no
certificate, source coverage or completion. Existing results are preserved apart
from discovery work counters in two pairs. Evidence:
`benchmark/realworld/remaining/source-completion/equal-domain.md`,
`equal-domain-v1-pilot.json` and `equal-domain-checks.json`. Next inspect actual
equal-candidate proof failures before expanding admission; keep the goal active.

OCR integration preflight found a Linux-only worker limit implementation. macOS
now uses the same fail-closed address-space/CPU/core limit calls; Windows remains
unsupported. Linux CLI tests pass 134/134, including an actual excess-reservation
child test; macOS ARM checks include test code, but runtime is unverified. No OCR
provider or strict completion is added. Evidence:
`benchmark/realworld/remaining/source-completion/worker-limits.md` and
`worker-limits-checks.json`. At the user's request, regenerable debug artifacts
were cleaned while retaining frozen binaries, inputs and observations.

Page-exterior paint preflight is complete on all 72 fixed inputs using the frozen
current native worker. Of 446,318 retained records, 841 are strictly exterior,
403,130 touch/overlap the page and 42,347 have unknown bounds. Zero paint-bearing
pages contain only exterior records. Excluding exterior paint alone cannot close
an observed page inventory; deprioritize that exception as a completion strategy.
All typed acquisitions succeeded but retain 18 issues. No production predicate or
completion changed. Evidence: `benchmark/realworld/remaining/source-completion/paint-extent.md`
and `paint-extent-results.json`. The goal remains active with zero new completions.

Completed content proofs can now own validated intervals while retaining
uncertain localization. Exact masks over the complete interpretation family are
required; count-only witnesses and potentially equal families remain nonowning.
The six-pair pilot adds one EDPB restrictions cut (103 old / 105 new references),
independently checked against frozen native readings and both spacing
interpretations. Five other comparison objects remain identical. All six remain
incomplete; zero newly complete pairs. Workspace: 2,506 passed, two ignored;
Clippy, formatting and generated fixtures 48/48 pass. Evidence:
`benchmark/realworld/remaining/source-completion/domain-content.md`,
`domain-content-v1-pilot.json` and `domain-content-source-review.json`.
Inventory and search remain independent obstacles. Keep the goal and plans active.

Internal SourceCut ownership is now implemented for plain matched native
populations, with renewed occurrence/endpoint proofs and explicit per-parent
remainders. Ordinary and padded boundaries, plus asymmetric retained paragraph
splits, pass source-conservation controls. The final six-pair pilot remains
identical to the preceding comparison objects and coverage; no new natural cut
certificate or completed pair is claimed. Workspace: 2,505 passed, two ignored;
Clippy, formatting and generated fixtures 48/48 pass. Evidence:
`benchmark/realworld/remaining/source-completion/source-cut-domain.md` and
`source-cut-domain-v3-pilot.json`. Other population conventions and unresolved
content/localization obligations remain; keep the goal and both plans active.

Follow-up correction: exhausting optional cut discovery no longer skips the
separately bounded interval proof for a range already found. Five constrained
discovery-budget regressions pass while cut searches remain incomplete. Workspace
tests pass 2,503 with two ignored; Clippy, formatting and 48 generated fixtures
pass. The same six natural comparison objects and coverage are unchanged, with no
new completion. Evidence: `benchmark/realworld/remaining/source-completion/native-interval-budget.md`
and `native-interval-budget-pilot.json`. Continue with internal SourceCut ownership
and the independent inventory/search obligations; the goal remains active.

Latest implementation result: independently revalidated whole-node native
intervals now supply conditional source ownership under two admitted source-only
endpoints. The six-pair pilot adds 830 old and 827 new compared references across
seven ECB intervals; the other five controls retain coverage, and all original
comparison objects are unchanged after excluding the new field. Workspace tests
pass 2,502/2,502 with two ignored; Clippy, formatting and 48 generated fixtures
pass. No newly complete pair. Independent source adjudication of these seven
intervals, internal SourceCut ownership, and full partition invariance remain
pending. The seven literal native readings and exact masks have subsequently
passed an independent source audit and a separate all-optimal-edit-path oracle
(225 exhaustive small controls). Boundary correspondence and paint closure remain
outside that audit. Evidence: `benchmark/realworld/remaining/source-completion/native-interval.md`
and `native-interval-pilot.json`. Keep the original completion gate and both plans.
The execution source of truth is this Markdown file. [Read the HTML companion](PLAN.html).

Current integration finding: the existing OCR route cannot discharge strict source
coverage even when its provider declares complete inventory and confidence 100.
Eight text-only integration cases now preserve this boundary for both equal and
changed readings. Production OCR integration alone is therefore not a path to
this goal's completion gate; prioritize independently validated source proofs and
acquisition closure. This does not establish that all possible future acquisition
proofs are impossible. See the strict completion integration check in
`benchmark/realworld/remaining/source-completion/ocr-probe/CPU.md`.

## Original problem and desired outcome

The fixed 36 natural PDF revision pairs currently have zero complete comparisons on the common `--channels text` route. Useful local findings exist, but they do not discharge all document obligations. The user wants completed comparisons to increase through a general implementation that can support all PDFs, without rules for particular documents or publishers.

The user explicitly chose a bounded first implementation increment: make several of the 36 pairs complete, then stop and report meaningful progress. Operationalize “several” as at least two newly complete natural pairs under the unchanged completion meaning, with no completion losses. General support for all PDFs and eventual 36/36 remain the long-term direction, not this increment’s acceptance gate. Source residual reduction alone with completion still at zero does not satisfy this increment. If a concrete access or scope constraint prevents that result, report the increment as unfinished and retain the plans.

“All PDFs” is an architectural requirement: the same evidence contracts and extension paths apply independently of file identity, producer, language, page count, and layout. A finite panel cannot prove universal PDF support. Malformed, encrypted without credentials, unsupported, or resource-limited input must remain explicit failures or unresolved evidence; it must never be made to pass by weakening a claim. Record representation-specific gaps so future acquisition providers can fill them through the neutral facade.

## Definition of done

- At least two distinct pairs in the unchanged 36-pair panel newly reach document-wide `comparison_complete: true` relative to the captured current baseline, with no previously complete pair lost, in two captures of the same final build, inputs, arguments, and budgets, with valid process outcomes and source-bound evidence. Evaluate the actual baseline first; a historical reported 0/36 is not a newly reproduced baseline.
- No known false A/B, false complete result, or incorrect strict source mask remains in evaluated outputs and controls. Re-adjudicate current events against current sources and premises; old matching digests alone are insufficient.
- The production path has no panel IDs, publisher names, expected text, input hashes, gold annotations, or fixture coordinates driving comparison decisions. Evidence-derived rules pass independent producer and layout controls plus a small newly frozen holdout evaluation. At least two independent producer families must exercise the new proof path through source-verified natural examples; they need not both become document-wide complete if independent inventory obligations remain.
- All five core acceptance cases pass: line-wrap and page-break invariance with no content change, and replacement, paragraph insertion, and paragraph deletion with exactly one expected change each.
- Relevant Rust, fixture, and evaluator checks pass. Durable artifacts retain before/after results, unresolved inventory, strict source and search obligations, costs, build identity, and adjudication. A synthetic proof or a local visual-equivalence result does not count as a natural complete comparison.
- Evaluate these results against the original failure; preserve evidence; clean up only this run's two planning files; then report completion.

The user clarified during planning that immediate 36/36 is not required: implement enough to complete several pairs, then end this increment with a progress report. The minimum of two makes that outcome measurable without claiming that all PDFs are supported. Once this outcome and its validation are established, stop this increment; do not continue toward 36/36 without a further request. C1–C5 describe a prioritized path, not a requirement to implement every proposed subsystem before reporting. Defer unneeded checkpoints explicitly with remaining evidence and rationale.

## Evidence established during planning

- HEAD is `4ef99cac98fa0e45e7cb5ed820c14b0b8a485704`, matching the supplied investigation.
- `crates/pdfdelta-core/src/document/coverage.rs` counts distinct source references, requires both inventories complete and zero uncompared references, and excludes inferred comparisons. Its set-based reporting must not be mistaken for a proof of multiplicity preservation.
- `crates/pdfdelta-core/src/document/comparison.rs` compares mandatory edges, retains competing-optimum and incomplete-search obligations, and exposes `search_resolved()`. `crates/pdfdelta-cli/src/evidence_compare.rs` requires both coverage completion and resolved search.
- `crates/pdfdelta-core/src/document/text_scopes.rs` explicitly makes `TextScopeReview` non-owning. Its source lists and cuts are range locators, not strict coverage or changed masks.
- `benchmark/realworld/remaining/current-gate-evidence-audit.json` records seven current range-hit candidates, only one of which retains the historical reviewed event identity. `ideographic-indent-panel-result.json` records 0/36 and a 28-reference comparison increase. These are repository observations, not panel reruns performed while planning.
- `benchmark/realworld/remaining/panel-obligations-result.json` already contains a metadata census. Extend existing evidence instead of introducing another general audit framework or rediscovering that census.
- `benchmark/realworld/source-boundaries/opaque/ADOPTION.md` records a restricted page-program experiment, no W-2 certificates, unresolved observer binding, and no change to inventory or completion. `irs-all-paint-result.json` retains actual paint differences.
- `registered-baseline-restoration-result.json` records unsuccessful byte-identical historical binary restoration. Historical missing raw reports and binaries remain evidence gaps; current diagnosis must not wait indefinitely for their recovery.
- Cargo is available here (`cargo 1.98.0`); the supplied external investigation's missing-Rust environment is not this environment. Its ZIP and detailed external artifacts were not found locally. The pasted 23-PDF, 210-component and 14-condition experiments are user-reported independent PoCs, not reproduced Rust or natural-panel evidence.

## Constraints and authorized boundaries

This handoff authorizes no execution by itself. A later invocation of the contract permits necessary local implementation in `crates/pdfdelta-core`, `crates/pdfdelta-cli`, `crates/pdfdelta-bench`, existing benchmark tooling, and directly related documentation. Use `benchmark/realworld/remaining/source-completion/` for new durable run evidence and ignored `benchmark/realworld/cache/source-completion/` for input copies, binaries and raw captures. Reuse capture/verify modules; add only a small versioned adapter where required. Do not overwrite historical manifests, results, plans, annotations, or gate meanings.

Preserve existing work: `Cargo.toml` and `README.md` have user changes; root `PLAN.md` and `PLAN.html` are already deleted; unrelated Python cache directories exist. Existing plans under `docs/plans/remaining/` and `docs/plans/source-boundaries/` are not owned by this run. Snapshot worktree/build differences at execution start; do not reset them. Earlier deadline, delegation permissions, stop records, and historical gain gates are historical context, not this run's execution policy.

Preserve raw evidence -> reversible imperfect structure -> robust shared alignment -> exact local diff. Keep parser-library types behind `PdfParser`/`ParsedPdf`; preserve glyph text/codes, geometry, render mode/order, and object/operator/Form provenance. Keep core free of CLI filesystem and process concerns. Use stable Rust, Edition 2024, relative geometry and explicit resource limits.

Local OCR is authorized subject to a Rust implementation, multilingual support (including Japanese and English), and cross-platform operation. Preserve recognition uncertainty, model identity, resource limits, and native/rendered/stored distinctions. Prefer explicit local model provisioning and no external analysis service. No new custom PDF object parser, semantic models, table recognition, or speculative performance redesign in this contract. A concrete fixture or measurement can motivate a separate scope decision for those capabilities; until then keep missing interpretation unresolved. Necessary fixes to existing acquisition and supported source proofs are in scope. Do not replace the solver, add a whole-document flattened alternate diff, promote B/C, infer deletion from unmatched candidates, or count inferred partitions as strict ownership.

Do not commit, push, publish, contact others, or start subagents under this contract. Public input reacquisition may use the registered source locations if necessary, retaining exact expected hashes and explicit failures. Changes to completion semantics or observation contracts require a separate user decision; a newly versioned appearance report cannot replace the original goal.

## Checkpoints

### C1 — Establish a reproducible baseline and all remaining obligations

- [x] Bind all 36 pairs and 72 expected input hashes from `benchmark/realworld/followup/panel.json`, frozen targets and controls, actual source/worktree snapshot, Cargo.lock, toolchain, build flags and binary digest. Keep an executable copy and raw reports with a manifest of exact paths and hashes.
- [x] Reuse only captures with verifiable identical inputs, executable, flags, budgets and contract. Capture missing current observations twice; use a new versioned current baseline if historical artifacts are missing, without declaring the old baseline restored.
- [ ] Extend the existing audit with three independent obligation classes: inventory, strict source accounting, and search. Each obligation carries side, source/domain locator, evidence pointer, proof dependencies, and status: proved, unresolved, or not evaluated. Unknown inventory size is not zero.
- [ ] Attach all applicable causes to each residual: acquisition, normalization, missing candidates, correspondence ambiguity, competing partition, endpoints, order, non-owning B, and paint interpretation. Overlapping causes are not additive source counts. If evaluation stopped, preserve unevaluated dependencies instead of inventing their status.
- [ ] Select the first shared cause by measured affected source and pairs, including joint blockers. Record which proposed proof can actually reduce a current residual. This census is a decision checkpoint, not the final deliverable.

### C2 — Build source-preserving comparison domains from existing cuts

- [ ] Introduce the smallest validated domain/proof representation near `document/text_scopes/cuts.rs` and its existing evidence/graph types. Keep discovery, validation and consumption separate; do not trust a serialized certificate without rebinding and validation.
- [ ] Partition each parent into equal anchors, interior comparison domains, literal boundary/space residuals, and unresolved fragments. Their union preserves the parent's actual ownership and scalar-to-source multiplicities exactly. Track repeated references separately from unique owned evidence; preserve multi-character glyph sharing as indivisible ownership components. A set equality alone is insufficient.
- [ ] Reconstructed spacing carries contextual references without owning the adjacent literal glyphs. Never split a ligature, lose an unresolved fragment, duplicate ownership through overlapping cuts, or enlarge a change mask to include anchors or unchanged parent context.
- [ ] Validate parent correspondence, exact endpoints, meaningful order, local inventory, normalization alternatives and rival partitions. A boundary interval alone cannot certify an owning domain. Alternative-partition selection remains inferred unless a separate compatible source proof establishes what survives every rival.
- [ ] Feed validated domains to the existing shared solver and exact local comparison. Before strict accounting adoption, document which existing source obligation the new certificate discharges and why existing correspondence conditions remain true. Keep non-owning B unchanged.
- [ ] Verify segmentation metamorphisms using identical raw evidence with different paragraph/line/page boundaries: provable coverage and unresolved ownership remain invariant, while exact masks follow the changed evidence only. Replay affected natural pairs and measure strict residual changes.

### C3 — Prove content independently of correspondence uniqueness

- [ ] Represent correspondence (unique/ambiguous/unknown), content (proved equal/proved changed/unknown), and localization (exact mask/range only/unknown) independently. Reuse existing local change-with-uncertain-position behavior; first implement only the additional equality proof.
- [ ] Require an independently validated, closed, ordered domain with complete local acquisition and source ownership. Do not infer scope or order from matching string bags. Retain keyed-value, membership, relation and move/copy obligations even if text is equal.
- [ ] Establish equality for every admissible alternative using exhaustive search, a proved conservative superset, or an independent domain theorem. Candidate truncation, source sharing, omitted rival partitions, uncertain normalization and resource exhaustion must not be treated as complete enumeration.
- [ ] Compare bounded exhaustive assignments with the new proof on repeated-content controls; include the eight-equal-items case and adversarial ordering/scope/key/value/count changes. Explicitly model whether arbitrary permutations are admissible under the actual order contract; do not transplant the PoC's 40,320 assignments into an order-constrained solver by assumption.
- [ ] Connect the certificate to exactly the content obligations it proves. Keep correspondence ambiguity visible; do not globally clear competing-optimum reasons or bypass `search_resolved()`. Discharge a search obligation only when its entire effect on the selected-channel result is proved invariant and its remaining independent obligations are represented.
- [ ] Measure applicable components and actual residual reduction on the fixed panel. If applicability is negligible, document it and prioritize a demonstrated shared blocker instead of expanding this mechanism speculatively.

### C4 — Localize paint execution dependencies without claiming text acquisition

- [ ] Extend the restricted experiment before production adoption. Distinguish invoked resources from unused resource entries, page/ownership back-references from real Form recursion, and effects from conservative influence footprints.
- [ ] Bind caller state, Form matrix and invocation, exact numeric provenance, clip/text clipping, mask, alpha, blend, backdrop, optional content, annotation appearance, output profile and nested resource contents. Same operator names with changed image bytes must invalidate equality. Unknown relevant state or possibly intersecting influence remains unresolved.
- [ ] Start with a demonstrably sufficient bounded primitive class; explicitly reject unsupported font, transparency and numeric cases. Distinguish failure of an equality proof from a proof of inequality. Raster matches are independent controls, never certificates.
- [ ] Verify disjoint body/widget/unused-resource changes; changed color, width, endpoint, caller/Form transform, active clip and image bytes; unknown overlapping Form/widget, missing resource, cycles, decimal collisions and limit exhaustion. Preserve actual Schedule C/SE paint differences and report W-2 applicability without promising success.
- [ ] Separately validate any native glyph/operator/Form invocation to renderer-observer binding before consuming it. Emit relative appearance evidence separately; do not set text inventory or existing `comparison_complete` from opaque equality.

### C5 — Close the actual inventory and integration gaps

- [ ] Recompute joint residuals after C2–C4. For every incomplete pair, identify the next evidence acquisition or proof obligation that actually prevents whole-text completion. Localized paint equality alone is not a solution to undiscovered text.
- [ ] Fix supported acquisition and local closure through existing parser/provider boundaries, preserving visibility and stored/native/rendered distinctions. A drawn minus, changed image text, occluded native text, and visible native replacement must remain distinct controls.
- [ ] If supported evidence can close an inventory, provide positive and adversarial source-backed tests and consume only that certificate. Add an authorized Rust multilingual cross-platform OCR provider where acquisition requires it; OCR confidence alone cannot certify exhaustive acquisition or a unique interpretation. Retain exact missing representations and unresolved recognition alternatives. Do not claim impossibility merely because the current implementation lacks a feature.
- [ ] Continue shared fixes until at least two newly complete pairs and the validation criteria are established. Then report this increment, including all remaining pairs and their next shared blockers; further work toward 36/36 belongs to a later increment. Do not spend the remaining work solely reconstructing an old binary hash.

### C6 — Prove natural-PDF improvement and generality

- [ ] Capture the fixed 36 pairs twice at the final frozen build. Retain identical input hashes, common `--channels text`, limit scale 1, 180-second capture timeout, and existing native defaults (35 seconds and 128 MiB response bound). Operational per-input limits are preserved, not a deadline for this goal.
- [ ] Count all 36 including failed acquisition, source-zero, timeout and unsupported results. Count completion only with valid exit 0/1, full selected inventories, zero strict source residual, and resolved selected comparison obligations. Report all-channel observations separately.
- [ ] Print per-pair and per-producer before/after completion, fixed target recovery, exact source residual, inventory residual, search residual, new/lost A/B, adjudication, runtime, peak RSS, report bytes and budget stops. Never label peak RSS as live allocated memory. Justify regressions and optimize only demonstrated costs.
- [ ] Re-review current source-bound events, including the six changed candidate digests. Preserve finite gold ranges, changed cores, multiplicity and unchanged controls; range containment alone cannot pass review.
- [ ] After implementation/build/configuration freeze, register a small fresh natural holdout of at least three pairs from at least two independent producers, including representation/layout variants the new proofs claim to support. Freeze source expectations before capture. Report every attempt and every claimed completion; a claimed-supported case left unresolved requires investigation. Do not tune on this holdout and still call it blind; promote exposed data to development and obtain a new frozen holdout if changes are needed.
- [ ] Run the five release acceptance cases and adversarial controls for repeats, move/copy, keyed value swaps, cross-page layout, literal/reconstructed spaces, glyph sharing, acquisition gaps, intersecting/disjoint paint, alternative normalization and exhausted budgets.

## Verification commands and evidence handling

Run from `/home/hayato/ghq/github.com/hayatosc/pdfdelta`. At execution start load `coding-style` and relevant Rust skills for the changes; use focused code search when callers are unclear. Planning did not run the Rust suite or natural panel.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p pdfdelta-bench --locked -- verify
PYTHONDONTWRITEBYTECODE=1 PYTHON_UV=0 python -m unittest discover -s benchmark/realworld/remaining -v
```

Use affected fixtures first, such as `cargo test -p pdfdelta-core --test document_text_scopes_fixture` and `cargo test -p pdfdelta-core --test document_groups_fixture`. Run the opaque-effect example tests if that experiment changes. Run broad checks at integration, reuse passing checks when source/build inputs have not changed, and fix failures introduced by this work.

Existing capture entrypoint: `benchmark/realworld/next/development/capture-comparisons.py`. Its positional arguments are executable, PDF cache, and a new output directory; flags are `--manifest`, repeated `--pair`, `--implementation`, and `--route text`. It requires frozen manifest-relative annotations. C1 must bind all fixed panel pairs to their original manifest/annotation locations rather than assuming `panel.json` has the capture script's schema. Use separate output directories for repetitions and record the complete resolved argument arrays. A working-tree build needs its patch/source snapshot in addition to the commit label.

Existing verifier entrypoint: `benchmark/realworld/remaining/verify.py` with `--stage`, `--contract`, and `--evidence-dir`. Existing contracts are `historical` and `source-boundaries-v1`; neither should be relabeled as this run's implementation-increment contract. Add only the minimal versioned extension needed to validate the new proof records and acceptance criteria. Retain old verifier results as historical results. A new adapter's final invocation is recorded when implemented, not presented here as an existing command.

Durable summaries must include exact raw artifact locations and content hashes, captured binary, source snapshot, lockfile, build command/environment and expected contracts. Hashes identify artifacts; they do not replace source equality proofs. Report missing evidence as missing. Keep necessary raw evidence until an explicit retention decision; do not delete all replay artifacts to reclaim disk without preserving what validates the result.

## Planning files owned by this run

- Markdown: `/home/hayato/ghq/github.com/hayatosc/pdfdelta/docs/plans/source-completion/PLAN.md`
- HTML: `/home/hayato/ghq/github.com/hayatosc/pdfdelta/docs/plans/source-completion/PLAN.html`

Neither existed before this run. Both are temporary and must agree on the objective, criteria and decisions. Record progress in Markdown and refresh the HTML after material plan changes. No remote preview server, tunnel or staged public copy is authorized or running. A local browser check does not publish the plan.

## Progress log

- 2026-09-16 — User decision: end this increment after implementation produces several newly complete pairs and verified meaningful progress; retain universal design as the direction. Plan uses a minimum of two newly complete pairs, with the full 36 retained in evaluation.
- 2026-09-16 — Planning: checked HEAD, existing work, completion/source/search paths, current audit, old gates, paint adoption limits and capture CLI. Confirmed Cargo availability. No implementation, natural-panel rerun, or new improvement claimed. Next: execute C1 only after a separate user request to run the goal.

- 2026-09-16 — Execution progress: preserved the current release binary and source archive; verified all 72 input hashes. Baseline driver is live as exec session 68807, capturing both repetitions; do not restart it while running. Added `remaining/audit_reports.py` and three regression tests, plus per-inventory and non-text paint page observations in CLI reports. CLI tests, CLI Clippy and 18 evaluator tests pass. Schedule SE proves that absent text issues do not imply complete inventory; paint and conflict-search obligations coexist. Durable evidence: `benchmark/realworld/remaining/source-completion/diagnostic-observability.json`. No new completed pair is claimed. Next: finish and audit both baseline repetitions, then bind individual residual sources to validated comparison domains.

- 2026-09-16 — C1 evidence: repetition 1 is fully audited at 0/36; inventory and source obligations are unresolved in all 36 pairs, search is unresolved in 34. The second repetition remains live in session 68807. `smallest-residuals.json` binds complete source/review bundles for Schedule SE and C: neither contains a non-owning B review, SE has a 143-proposal component stopped before subset search, and C includes repeated labels plus real keyed-row swaps. This rules out simply feeding existing B reviews or equal text bags as a solution for the two smallest residual pairs. Next: finish baseline audit using `/tmp/pdfdelta-audit-source-baseline.py`, retain the final registration, then implement source-preserving domains with independent correspondence/ordering premises; keep inventory work separate.

- 2026-09-16 — Baseline complete: both 36-pair repetitions finished and were audited. Both are 0/36; coverage and the three obligation statuses agree pair-by-pair. Input, original target and control hashes remain registered. No baseline process remains live.
- 2026-09-16 — Source partition progress: added `TextSourcePartition`, a borrowed, non-deserializable ownership partition with explicit complement sources/token ranges and unchanged origin multiplicities. SourceCut slices and literal-edge padding now use it with shared work accounting. Five new controls cover every cut of a shared-glyph view, synthetic-only intervals, malformed/resource-limited input, nested context references, and exact local masks excluding the complement. This is the accounting primitive, not yet a corresponding multi-node domain or a strict coverage certificate.
- 2026-09-16 — Checks and natural pilot: 2492 workspace tests passed (two ignored), workspace Clippy and format passed, and generated fixtures passed 48/48. EDPB controller/processor, NIST incident handling and Mask R-CNN retain identical strict comparisons and coverage. Existing B reviews are preserved; Mask has one additional out-of-target B range. Its token-count witness was checked against retained native glyphs (digit 0: two old occurrences, one new), with the existing range-event validator. No fixed target or completion gain is claimed. Evidence: `partition-pilot.json` and `partition-added-range-review.json` in `benchmark/realworld/remaining/source-completion/`. Next: construct corresponding domains across parent partitions and validate order, inventory and rivals before any strict source discharge; the independent all-panel inventory deficit remains.

- 2026-09-16 — User authorized local OCR provided it uses Rust, supports multiple languages and runs across platforms. Provider selection and integration must preserve model provenance, bounded execution, ambiguous recognition and source conflicts. This extends acquisition scope; it does not relax strict inventory/source/search completion or count OCR output as a new completed pair.

- 2026-09-16 — OCR feasibility: an isolated Rust RTen 0.26.0 probe correctly recognized known Japanese and English crops using local PaddleOCR models; a detection-model smoke test produced finite probability output. Linux Clippy, Windows GNU target checking, and Apple ARM64 target checking passed. Other-OS runtime execution remains unverified. The probe and hash record are in `benchmark/realworld/remaining/source-completion/ocr-probe/`; no production dependency or completion predicate changed. The existing worker process limiter rejects non-Linux platforms, so production OCR must address platform-compatible bounded execution instead of inheriting that restriction silently. Next: implement and validate bounded region acquisition with model identity and uncertainty, then connect only independently proven obligations; strict completion remains 0/36.

- 2026-09-16 — User additionally requested CPU practicality evaluation. Measure full-page detector plus recognition work on known Japanese/English pages and natural IRS/research pages, at one and four CPU threads with repeated identical runs; retain time, peak RSS, raw recognition output, failures and generated-text accuracy. Separate PDF rendering/native comparison costs and cross-platform compile checks from actual runtime observations. Do not infer full-corpus throughput or correctness from the isolated crop timing.

- 2026-09-16 — CPU evaluation completed: 36 sequential prototype runs across six page cases, one/four threads and three repetitions. On a Ryzen 5 5600X, four-thread median time was 2.87–3.53 seconds for generated pages and 3.13–8.33 seconds for sampled natural pages, with at most 130.2 MiB RSS. Shared-host timing varied substantially. English matched all 30 generated lines; Japanese retained 24 character edits even after ignoring whitespace for diagnosis. Complete text inventory is not established. The panel has 5,684 old/new pages; region-focused acquisition is preferable to unconditional full-page OCR, subject to retained omission/overlap obligations. Durable evidence: `ocr-probe/CPU.md`, `cpu-analysis.json` and preserved raw/cache artifacts. Goal remains active at 0/36; production OCR and source/domain discharge are still outstanding.

- 2026-09-16 — Residual applicability preflight: Schedule C has 30 exhaustive ambiguous components consisting only of non-inferred literal leaf proposals. Their combined currently uncompared source opportunity is at most 160 references per side, before validating domain closure, inventory, order, rival partitions and dependency closure. Schedule SE has no such component. This is not an equality certificate or coverage gain; the retained C residual is 1,298/1,304, so repetition equality alone cannot close either smallest pair. Evidence: `content-invariance-preflight.json`, bound to source/report hashes. Prioritize corresponding SourceCut domains and independent inventory obligations over expanding the repeated-content fast path. No production state changed in this preflight; goal remains active.

- 2026-09-16 — Implemented the first strict ownership connection: admitted source-only padding boundaries can supply `NativeTextDomainEquality` after whole-parent local closure and source-preserving cuts. Owned native bodies enter text coverage; complements, paragraph identity, search and global inventory remain unresolved. Proof authority is not deserializable. All 2,493 workspace tests pass (two ignored), Clippy/format pass, generated fixtures pass 48/48, and eight new integration scenarios preserve unsafe exclusions.
- 2026-09-16 — Natural pilot: controller/processor gained 694 strict references on each side (12 domains); restrictions gained 597 on each side (14 domains). Schedule C/SE and NIST contingency stayed unchanged. Independent native-source reconstruction verified all 26 domains and their complements. Existing strict comparisons and B reviews were preserved. The extra three restrictions B reviews also occur with the earlier preserved partition binary. Evidence: `native-domains.md`, `native-domains-pilot.json` and both source-review JSON records. All five pilot pairs remain incomplete; no full-panel completion gain is claimed. Next: expand corresponding domains and address inventory through execution-state proofs of genuinely absent paint effects, retaining provenance and every unresolved interpretation; local OCR remains a separate pending acquisition path.

- 2026-09-16 — Empty-clip applicability measured with a separately preserved diagnostic binary across all 72 fixed inputs. Three execution controls pass, including pending-clip timing and q/Q restoration. Logs contain 21 empty-clip paint-record attempts across four documents; every affected page has other paint attempts. One native acquisition returns the retained CID-width resource limit; successful native replies may still contain issues. Attempts can include subsequently rolled-back Form effects, so no inventory closure or source gain is inferred. Deprioritize an Empty-clip-only production exception; broader acquisition/domain work is still required. Production source and release executable restored. Evidence: `clip-probe.md`, `clip-probe-results.json`, diagnostic patch and driver. Goal remains active at 0/36.

- 2026-09-16 — Fixed native-domain proof work accounting: the shared exact comparator now charges actual work to the domain ledger instead of a quadratic grid reservation. The previous estimate overcharged long equal bodies and underfunded short ones (four versus ten units for one token per side). Shared-budget exhaustion and short-boundary regression tests pass. Workspace: 2,495 passed, two ignored; Clippy, formatting and generated fixtures 48/48 pass. Same five-pair pilot adds 54 strict references per side for controller/processor and 296 per side for restrictions over the previous implementation; the other three are unchanged. All prior candidates, comparisons, reviews, domains and unresolved reasons are retained. Independent reconstruction checks all 78 domains against preserved source bundles. No newly complete pair: inventory and search still remain. Evidence: `domain-work.md`, `domain-work-pilot.json` and both source-review records.

- 2026-09-16 — Corrected a missing native-projection check in new domain ownership. A pre-fix counterexample changed native text while retaining graph text and still owned the affected range. Domains now use the existing complete-glyph/order validator and source-checked expansion of contracted literal edge spaces; interior source changes and unresolved interpretation remain excluded. Workspace: 2,496 passed, two ignored; Clippy, format and generated fixtures 48/48 pass. Final same-five-pair pilot retains 32 controller/processor and 24 restrictions domains, with baseline gains of 714 and 571 references per side. These supersede the prior 748/893 gains; 34/322 references return to unresolved with the added requirements and unchanged budget. Other local comparisons, candidates, reviews and search reasons remain unchanged. The retained domains are a subset of the prior independently audited source witnesses. No newly complete pair. Evidence: `domain-projection.md` and `domain-projection-pilot.json`; intermediate contracted-padding rejection is separately archived. Goal remains active.

- 2026-09-16 — Restored ECB annual 2023 native acquisition through shared CID width resources. A read-only parser-facade probe found nine Type0 fonts referencing one descendant table; repeated expansion hit the 65,536 retained-entry ceiling. A PDF-bound cache now shares only identical descendant horizontal tables; per-font mappings, IDs, geometry, provenance, vertical costs and all numerical limits remain. Failed loads never publish tables; cache/backend versions were updated. ECB old acquisition now yields 462,245 glyph records. Two same-build/inputs/arguments/budget captures agree exactly on comparison, coverage and evidence summaries, with 45,097 old / 45,085 new strictly compared references. Inventory/source/search still prevent completion. The preceding five controls retain identical comparisons and coverage. Workspace 2,500 passed, two ignored; Clippy, formatting, fuzz-feature check and generated fixtures 48/48 pass. Evidence: `cid-width.md` and `cid-width-pilot.json`. No newly complete pair; full goal remains active.

- 2026-09-16 — Native denial trace: two diagnostic captures preserve comparison and coverage exactly. All six controller/processor equal candidates fail paint closure; Schedule C path [39,40,41,42] fails the descending-baseline check. Reused the existing spatial row census for whole-node interval ownership, retaining full population/paint checks and rejecting clipped padding. A side-by-side repeated-text regression fails before and passes after; omission and reversed-order controls reject. Affected tests 90/90, generated fixtures 48/48 and workspace Clippy pass. All six natural pilot comparisons and coverage remain identical; no new completion. Evidence: native-denials.md, row-domain.md and their JSON registrations. Removed 103,856,344 bytes of completed macOS cross-check build artifacts while retaining evidence. Next: isolate downstream local paint/acquisition obligations rather than infer success from admitting an earlier boundary/order condition. Goal remains active.

- 2026-09-16 — Inventory applicability audit reused hash-verified frozen paint projections and operator logs instead of rebuilding extraction. Of 446,318 retained paints, 440,078 are path fills/strokes, 5,442 are XObject invocations, 610 shading and two inline images; 186 old ECB records remain unmatched after the historical acquisition failure. No finite bound has zero width/height. Only W-9, Schedule C and Schedule SE have solely path paint in both revisions. Image-only OCR cannot resolve these inventory obligations, and paths cannot be discarded as decoration. Evidence: paint-operator-attribution.md/json. No production predicate or natural completion changed. Next work must distinguish actual path-symbol interpretation from raster acquisition and relative paint equality; further candidate additions do not address the inventory prerequisite. Goal remains active.

- 2026-09-16 — Reconstructed six controller/processor old-side bands from retained glyph/paint data: unknown closed-stroke bounds contaminate figure-page ranges, and opcode alone does not distinguish curves from straight closed paths. Inspection found a prerequisite correctness bug: ExtGState LW was ignored, including cached resource reuse. The selection cache now applies optional width with optional font; invalid widths remain unresolved and hairlines keep unknown bounds. Cache format 15/backend v11 prevent stale width reuse. The generated regression reproduces widths [2,2,2,7,7] before and validates [2,30,2,30,30] after. Workspace 2,512 passed, two ignored; Clippy, formatting and 48 generated fixtures pass. All six pilot comparison/coverage records remain unchanged, no new completion. Evidence: gstate-width.md/checks/pilot and native-denial-paint-bands.json. Clarification: raster OCR can also receive path-drawn text; the operator census did not prove a separate path recognizer necessary. Next: bound positive-width joined/curved strokes using retained miter state, keeping unknown/intersecting paint and global inventory unresolved. Goal remains active.

- 2026-09-16 — User authorized commits. Recorded the width defect reproduction as e1427a1 and committed accumulated core/CLI implementation as 5350325 after passing 2,513 workspace tests (two ignored), Clippy and formatting. Joined/curved stroke bounds now retain miter state; automatic stroke adjustment and hairlines stay unknown. Generated fixtures pass 48/48. Six-pair pilot: Schedule C +3 source refs/side (raw reading "23 " independently checked); controller/processor +6 early-domain refs but -67 from its last two prior domains, net -61/side. The budget explanation is not yet established. All six remain incomplete. Retained 33 controller domains independently pass reading/source-conservation checks. Evidence: joined-stroke.md, checks/pilot and source-review JSON. Next: isolate the late-domain losses without increasing budgets, then continue inventory/search work. One-time plans and unrelated root changes remain outside commits. Goal remains active.

## Original-purpose evaluation

Pending. At closeout, compare the final repeated natural-panel results with the original 0/36 failure, explain why newly discharged obligations satisfy the unchanged completion meaning, and state how independent controls support generality. Checklist completion and smaller residuals do not establish resolution. If fewer than two newly complete pairs are established or the required correctness/generalization evidence is missing, continue within scope or report a concrete blocker; do not lower acceptance criteria. Otherwise end this increment and explicitly report the remaining incomplete pairs, evidence gaps and prioritized next work; do not imply that 36/36 or universal support has been achieved.

## Baseline-band index progress

The domain-budget trace reproduces the controller/processor loss as local proof exhaustion. A validated page-local baseline index restores both lost certificates without increasing limits or changing closure predicates. The six-pair pilot adds 31, 181 and 329 compared references per side for ECB, controller/processor and restrictions respectively; the other three counts are unchanged. All remain incomplete. Independent audits verify 70 controller domains, 42 restrictions domains and the added ECB literal change mask. Workspace tests pass 2,514/0/2 ignored, generated fixtures 48/48, Clippy and formatting pass. Durable records are under `benchmark/realworld/remaining/source-completion/baseline-index*` and `domain-budget*`. Continue with inventory/source/search residuals; no new completion or final repeated panel has been established.

## Full-panel recheck progress

The frozen d0e0fff implementation was captured once on all 36 pairs with all 72 input hashes verified and unchanged limits. Completion remains 0/36. Compared source counts increase for 16 pairs and decrease for none versus the first baseline; ECB includes restored acquisition. Inventory and source obligations remain for all 36, search for 35. Schedule SE's unresolved 143-proposal component is entirely inferred and solving it alone cannot discharge strict source residuals. Durable evidence: `completion-recheck-v2.json`, `completion-recheck-v2-audit.json`, `completion-recheck-v2.md` and `se-search-recheck.json`. This is not final two-run acceptance; prioritize the remaining acquisition/interpretation proof gap alongside source obligations.

## Type 3 code-local acquisition progress

Unused Encoding Differences names missing from CharProcs no longer reject defined codes in the same font. Used missing codes remain unresolved even with ToUnicode; identity cannot bind absent procedures. GPT-3 acquisition gains 11,527/10,275 glyphs and removes eight text issues; compared sources increase 9,354 per side. Newly discovered residuals remain explicit. Schedule SE comparison and coverage are unchanged. Both pilot pairs remain incomplete. Final checks: 2,515 workspace tests passed, two ignored, 12 Type 3 tests and 48 generated cases passed; Clippy and formatting pass. Reproduction commit `7d24366`; durable evidence `type3-code-gaps*`. Continue the acquisition/interpretation and strict source obligations; this does not establish a newly complete pair.

## Invocation-selected drawing dependencies

The benchmark profile `opaque-invoked-resources-v2` limits explicit resource acquisition to invoked `Do`/`gs` names at each page/Form scope, while refusing implicit default colour-space overrides. Four unused-resource positives and two default-space negatives extend the 24 existing controls to 30 passing conditions. Actual recursion and image changes remain dependencies. W-2 retains 11/11 unresolved pages due to unsupported AcroForm/annotation context. Whole-page command and context equality remains required; no spatial isolation, native/renderer binding, text inventory or completion claim is added. Workspace tests 2,515 passed, two ignored; fmt/Clippy pass. Durable evidence `invoked-resources-v2.json` and `.md`. Continue the interpretation/inventory proof gap; this experiment is not production adoption.

## OCR detector connectivity prerequisite

The isolated Rust CPU prototype now retains eight-connected foreground regions under unchanged limits. Two component tests and all checks pass. Six same-input/model four-thread observations preserve both generated transcriptions, consolidate seven SE regions at each resolution and one Mask R-CNN region at 72 dpi; natural recognition quality is unadjudicated. This remains an axis-box prototype, not full DB contour/polygon processing, production OCR or exhaustive inventory. No new complete pair is added. Frozen evidence and limitations are in `ocr-probe/connectivity-checks.json` and `CONNECTIVITY.md`; the previous repeated CPU results are preserved. Next acquisition work still requires complete postprocessing, alternatives and visibility/source proof.

## Current Schedule SE residual incidence

A fresh current-binary review joins all 867/884 residual native references to actual graph/proposal ownership. Every reference has only inferred candidates in the incomplete component and no mandatory candidate. They occupy 11 full native-layout text nodes per side; their whole-node readings have no cross-revision common value. Equality-only domain work and candidate-count increases cannot alone discharge this measured residual. Prioritize source-backed boundaries/identity for changed domains while retaining independent paint inventory. `se-residual-v2.json` and `.md` preserve the full incidence, evidence hashes and limitations. No new completion or contract change is claimed.

## Schedule SE discovery and mixed baselines

A read-only production trace confirms all residual nodes reach ordinary, row and native-structure discovery. Comparison and coverage exactly match the uninstrumented capture; production source and executable were restored. Seven residual node rectangles per side contact no retained paint, without proving closure of larger boundary intervals. Nodes 80, 84 and 87 have upward native baseline transitions at footnote digits/fraction numerators in both revisions; node 52 does not. Next isolate mixed-baseline order checks with a source-preserving fixture, retaining raised positions and rejecting ambiguous stacked text. Evidence: `se-runs-v1.json` and `.md`. No completion gain is claimed.

## Raised inline source projection

The failing reproduction was committed as `d8e751f`. Source-preserving mixed-baseline order recovers a strict synthetic interval while retaining exact masks and rejecting overlapping or separated glyphs. Four broader revisions lost two EDPB B reviews and were rejected. The final implementation applies mixed-baseline projection only inside already-bounded strict intervals, preserving discovery. Its three-pair pilot exactly preserves all comparison objects and coverage: Schedule SE 4,291/4,291, Schedule C 5,015/5,015, EDPB 2,240/2,266; all incomplete. Evidence: `inline-source-v6-pilot.json`, `inline-source-checks.json`, `inline-source.md`. No natural gain is claimed. Continue identifying Schedule SE boundary/closure obligations; synthetic superscript success does not explain or discharge the complete natural residual.

## Actual Schedule SE closure failures

A revision-bound trace records 52 old-side closure attempts: 31 reject paint contact, 20 reject node order and one rejects a non-monoline endpoint. New-side census is short-circuited, not validated. Independent full-band reconstruction shows 4–9 paint contacts for six paths, including the two amount-cell paths that already fail order. The 74/75 footer path has zero paint contacts but exact same-baseline, disjoint nodes rejected by the legacy order. Residual nodes 7, 76 and 99 never enter these closure calls. Evidence: `se-closure-v2.json` and `.md`; comparison and coverage exactly match production and the instrumented source/binary were restored. Next distinguish absent/terminal boundary premises from rejected censuses and test general same-row population closure. Do not assume superscript projection or more candidates can discharge the measured paint obligations. No new completion is claimed.

## Tagged page regions connected to strict intervals

Reproduction `254c670` shows that disjoint same-row nodes inside a tagged page sequence fail strict interval ownership. The implementation reuses the existing spatial row census per region, records its convention and reacquires native membership before reconstructing owned intervals. Exact mask and overlap/missing-source/reversed-tag/unknown-paint controls pass; workspace 2,517 passed, two ignored, Clippy/format and generated 48/48 pass.

A complete single fixed-panel recheck still reports 0/36. The current change gains 55/70 strict sources on IRS C and 57/72 on IRS SE, with one new B review each and no lost review signature anywhere in the panel. Source/mask oracles verify both footer/date intervals. Historical SSDF +517/+512 and ECB -31/-31 differences both reproduce exactly on the preceding frozen executable: the SSDF body-word removal is an earlier inline-projection gain; the ECB residual remains an earlier budget/proof-order issue. Do not attribute either to the new tagged-region connection. Evidence: `tagged-row-regions.md`, `tagged-row-regions-v1-audit.json`, `tagged-row-regions-v1-panel.json` and `tagged-row-regions-v1-panel-audit.json`. No new completion or fixed-body-target recovery is claimed. Continue remaining inventory/source/search obligations and diagnose the earlier ECB source loss without using elapsed time as a stopping rule.

## Paint declarations do not discharge inventory

The existing paint probe now retains inline ActualText, artifact markers, unresolved named properties and marker balance. Six fixed IRS documents (W-9, C and SE revisions), 20 pages and 1,215 paint operations have no inline ActualText or named-property frame at paint; 1,029 paints are artifact-tagged. These tags are not absence-of-text evidence. The CLI control keeps outlined text unresolved with and without an Artifact wrapper. Structure dictionaries, invoked Forms and annotation appearances remain outside this page-program diagnostic. No new completion or acquisition closure is claimed. Evidence: `paint-properties-v1.json` and `paint-properties.md`. Formatting, Clippy, 2,517 workspace tests, two probe tests and the extended CLI control pass. Continue source-bound acquisition investigation; an inline-declaration-only provider has no candidates for this measured paint population.

## Bounded interval lookup restores source coverage

A temporary ECB trace reproduced exhaustion before a previously owned 31/31-source interval. Whole-node interval preparation now shares each immutable graph-node lookup within one append call, charging construction and lookups while retaining all native, boundary and exact-diff checks. The five-pair pilot gains 151/150 ECB references, including the restored heading and two additional intervals. SSDF, Schedule C/SE and EDPB comparison and coverage remain identical. An independent raw-native/all-optimal-mask audit validates the three readings and masks (costs 2, 2, 1), with 225 oracle controls. Workspace tests 2,518 passed, two ignored; related tests 94, generated cases 48, formatting and Clippy pass. Evidence: interval-budget.md, interval-index-v1-pilot.json and interval-index-v1-audit.json. All five remain incomplete; no full-panel or final two-run completion gain is claimed. Continue acquisition closure and remaining source/search obligations under the original goal.

## Observation completion extension approved

A source-bound audit rechecked all 36 retained panel reports and the five latest pilot reports: every pair still has incomplete text inventory. The current OCR route is inferred even with confidence 100 and declared complete inventory; opaque rendering equality cannot interpret its text. inventory-contract.md records these boundaries and the approved, separately versioned observation extension. The user approved adding the separately versioned indicator on 2026-09-16. Implementation is pending and no predicate has changed. This addition does not replace the strict two-pair goal. Do not ask for OCR permission again, promote uncertainty, count observations as old strict completion, or assume that every future source-backed acquisition method is impossible. Keep both plans and the original goal active.

## Natural equal-domain applicability measured

The mixed fixed-panel snapshot contains 37 equal whole-node intervals and 3,138 exhaustive nonempty components without mandatory edges. Only one component is wholly enclosed on both sides by one such interval: Llama 2 scope 0 component 413, with 18 references per side. The scope still has incomplete text enumeration. This is a necessary-condition screen, not a search-discharge proof. A fresh current-binary Llama capture additionally measures the preceding lookup fix: 15 more equal intervals and +113 compared references per side, all prior intervals retained and the rest of the comparison object unchanged. Evidence: content-invariance-v1.json and content-invariance.md. No new completion, full-panel rerun, independent transcription adjudication or contract change is claimed. Lower the priority of the first equal-domain search consumer and prioritize independently justified acquisition/interpretation; do not infer that future content-invariance proofs are impossible. The observation-contract extension has since been approved; its implementation remains pending and existing strict results remain separate.

## Strict completion impasse

The unchanged strict two-pair objective is not achieved. The inventory-contract audit, equal-domain applicability follow-up and final source-state recheck retain the same acquisition-proof gap across three consecutive goal turns. Existing native evidence, inferred OCR and opaque relative equality cannot discharge it under the authorized contract. Independent source fixes and the measured equal-domain applicability investigation are complete; no presently justified implementation route to two strict completions has been established. Further advancement requires additional authoritative acquisition evidence or an explicit decision on a different comparison contract/target. The user subsequently approved adding the observation indicator. This opens the separate implementation path; adding it alone does not replace or fulfill the strict goal. Record this as an impasse, not completion, not proof of universal impossibility and not a confidence-threshold exception. Keep the original objective, frozen artifacts and both temporary plans; do not perform completion cleanup.

## Blockers and resumption

The missing external ZIP does not block repository-based implementation. Do not cite its experiments as reproduced. Missing historical executables/reports require a transparent current baseline, not fabricated restoration. Input access, required text interpretation outside the authorized scope, or an essential missing observer/provenance interface may become concrete blockers only after recording source evidence, attempted compatible approaches and the precise needed access or decision. Complete independent work while such a decision is pending.

No elapsed-time, turn-count or retry-count cutoff governs this goal. A tool timeout or harness interruption is not completion. Record the last validated source/build, commands, results, remaining obligations and next action, retain both plans, and resume from that evidence. Repeated failure calls for a revised hypothesis, not blind retries.

## Completion report and cleanup

1. Evaluate the implemented result against the original problem and all completion criteria, reusing valid evidence. If unresolved or inconclusive, update this plan and continue; report a specific blocker only when needed.
2. Preserve the original problem, result, per-pair evidence, controls, limitations and reproducibility references in durable benchmark artifacts and the final report, not solely in these temporary plans.
3. Close any later recorded owned preview server/tunnel and delete its exact staged copy and empty temporary directory first. Preserve unrelated sessions and files.
4. Delete only the two exact run-owned Markdown and HTML paths above, without wildcards, name-based searches or recursive directory deletion; verify both are absent. Preserve unrelated and later user edits. If ownership or cleanup is uncertain, report cleanup unfinished.
5. Only then mark the goal complete and report the outcome, evidence, limitations and removed paths. Never link deleted planning files as completion evidence.

## Runnable handoff contract

```text
/goal Resolve pdfdelta's 0/36 common-text completion failure through general source-backed comparison, achieving at least two newly complete pairs with no completion losses on the unchanged fixed 36-pair natural-PDF panel in two same-build, same-input, same-argument and same-budget captures. Preserve strict inventory/source/search completion semantics, all five release acceptance cases, exact masks, explicit unresolved evidence, resource limits and producer-independent behavior; verify source adjudication, independent controls, a small newly frozen holdout evaluation and project checks.
Follow /home/hayato/ghq/github.com/hayatosc/pdfdelta/docs/plans/source-completion/PLAN.md; its companion is /home/hayato/ghq/github.com/hayatosc/pdfdelta/docs/plans/source-completion/PLAN.html. Both are temporary planning files created for this run.
Implement and record meaningful progress within the plan's local crate/benchmark/documentation boundaries, preserving existing work. Local OCR is authorized only with Rust, multilingual support and cross-platform operation, while retaining recognition uncertainty and resource limits. Local commits are authorized. Do not publish, delegate, add custom parsing/semantic models, or change the completion contract without separate authorization. Continue based on evidence without elapsed-time, turn-count or retry-count cutoffs; lower residuals alone are not sufficient. Once the at-least-two-pair gain and required validation are established, end this implementation increment and report progress plus the remaining blockers; 36/36 is not required for this increment.
After the checkpoints, evaluate the original 0/36 problem against the recorded criteria. If unresolved or unproven, continue or report a concrete scope/access blocker with evidence and remaining work; retain both plans and do not claim completion.
After successful evaluation, preserve the result and evidence in durable artifacts and the final report, close recorded owned previews and remove their staged copies, then delete only the two exact run-owned planning files and verify their absence. Mark the goal complete only after evaluation and cleanup; report the original problem, resolution, evidence, limitations and deleted paths.
```
