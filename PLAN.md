# Demonstrate source-backed prose recovery on fixed and unseen inputs

Status: stopped with an unmet G3 constraint; goal not achieved.
Execution started 2026-09-11 07:28:35 UTC; original hard stop 2026-09-11 19:28:35 UTC.
Stop evidence: `benchmark/realworld/followup/RESULTS.md` and `constraint-stop.json`.
Plan location: `/home/hayato/ghq/github.com/hayatosc/pdfdelta/PLAN.md`.
Human-readable companion: [PLAN.html](PLAN.html). This Markdown file is the execution source of truth.

## Objective and definition of done

Expand source-backed comparison of ID-free prose, demonstrated on references
fixed before implementation and on a fresh blind set, while increasing complete
common-text document comparisons under unchanged budgets.

All gates below are mandatory. Passing tests, adding modules, reducing charged
work, generating reports, or discovering unannotated positives does not substitute
for any gate.

| Gate | Required observation |
| --- | --- |
| G1: development recovery | Additional A or B recovery on fixed ID-free body targets in at least six distinct natural PDF revision pairs from at least three independent publishers/producer families. A pair counts once, regardless of route or number of outputs. |
| G2: blind recovery | After implementation/executable/configuration freeze, a new 12-pair blind set yields additional A or B on fixed ID-free body targets in at least three distinct pairs from at least two independent publishers/producer families. |
| G3: complete comparisons | On one preregistered real-PDF panel and the common `--channels text` route, at least two additional distinct pairs become document-wide `comparison_complete: true` relative to the run baseline, with no losses of previously complete panel pairs. Same bytes, limits, channel selection and completion contract. |
| G4: correctness | No known false A/B remains in evaluated targets or admitted new A/B results; no false strict masks on fixed unchanged controls; existing exact regression/acceptance contracts pass. Evaluate A and B separately, and never add C to either recovery count. |
| G5: evidence | Per-family metrics, failures, complete-rate denominators, costs, source review and all mandatory quality checks are retained and printed by a fail-closed completion check. |

G1/G2/G3 are the user's newly agreed thresholds, not numbers retroactively
attributed to the original task. Full all-channel completion is measured
separately but is not the G3 gate. General success on every PDF is not required.

## Baselines and fixed evaluation

- Working baseline: `a2904f2bf5a708ddf38ddc52b90f2adaedb2acea`; production source
  currently equals `9093cab`. Preserve the original `3f318d7` history and results.
- Verify actual HEAD, worktree and existing planning files at execution start;
  preserve any later user changes instead of resetting the repository.
- Keep old manifests, annotations and results immutable. The exposed blind set
  may be used for diagnosis, but record its promotion to development before
  using its results to change production or comparison configuration.
- Select and freeze the development/completion panel before the first production
  change. Reuse the 36 acquired development/exposed-blind pairs where possible.
  Use at least 12 natural revision pairs spanning the six recorded families,
  English/Japanese, and at least three independent publishers/producer families.
  At least six panel pairs must contain suitable ID-free body targets. Do not
  retrospectively shrink the panel to successes or substitute synthetic PDFs.
- Capture the baseline on that panel. Existing exact-hash captures can be reused
  when their input, executable, flags, limits and result contracts match; rerun
  only missing observations. Count only baseline-missed targets as additional
  recovery and baseline-incomplete pairs as new complete comparisons.
- Freeze one or more independently authored source targets per eligible pair,
  including literal quotes, hashes, both finite source extents, changed core,
  permissible unchanged context, comparison convention and unchanged controls.
  Invariant content and scalar/source multiplicity must remain explicit.
- A recovery requires exact event/source gold where the positions are uniquely
  supported. B recovery requires coverage of both target cores, all necessary
  correspondence premises, and a range inside the predeclared permissible source
  extents. A whole-page span cannot pass merely by containing the target.
- Report strict event/source precision and recall separately. Mark unavailable
  gold and zero-detection precision undefined. Do not treat a missing annotation
  or failed acquisition as a successful empty comparison. Adjudicate every new
  A/B output in gate-counted pairs; retain additional out-of-target findings
  separately from frozen recall.
- Reuse the existing 60 content/layout controls and three real-source metamorphic
  controls. Their content and presentation axes stay independent. Include explicit
  positive and negative cross-page, column, move/copy and acquisition-gap cases
  for any newly admitted correspondence rule.

## Scope and diagnosis

The first decision is which necessary premise blocks each fixed target, not
which algorithm to add. Trace acquisition -> normalization -> scope -> retrieval
-> optimization -> counterpart decision -> localization -> reporting/evaluation.
Use retained reasons and caller flows. Add only missing bounded observations.

Current evidence suggests three hypotheses to test, not three predetermined fixes:

1. Acquisition/resource failures can leave no eligible local comparison even when
   candidate enumeration reports complete. Diagnose NIST zero-comparison records
   and native/common inventory differences before touching matching scores.
2. Existing native B closure requires a same-page horizontal source band and two
   accepted unchanged boundaries. Identify whether the fixed body targets fail
   source coverage, order, boundaries, local proof, or budget. Cross-page/column
   support is justified only by a concrete failed target and a valid source proof.
3. Candidate, descendant-ownership and conflict work can stop useful comparison.
   Optimize the shared root cause using the same candidate universe, compatibility,
   integer scores and source-conflict semantics. Do not raise limits to pass G3.

Choose one cause per iteration by expected impact on the frozen failed targets,
evidence strength and proof risk. Check the smallest regression first, then the
affected real pair. Broaden to the fixed panel only when the change merits it.
Keep performance-only changes separate from extraction/correspondence changes.

## Constraints and boundaries

- Production remains stable Rust, Edition 2024. Reuse the neutral parser facade,
  Evidence Store, reversible graph/groups, shared solver, exact partial assignment,
  dummy choices, scoped key presence, normalization proofs and local dependencies.
- Preserve raw glyphs/codes, geometry, render state, paint order, object/operator
  provenance and unresolved evidence. Never reinterpret similarity, an optimizer's
  necessary edge, or nearby geometry as source identity.
- B remains non-owning and C remains inferred. Do not promote an inferred parent,
  use dummy selection as deletion proof, flatten full documents into another diff
  path, or add context to strict masks/coverage.
- No runtime OCR, external analysis API, custom PDF object parser, semantic model,
  table-recognition expansion, pair-specific matching rule, expected-ID special
  case or annotation-text special case.
- Keep P2 pricing test-only and P6 strict observer binding deferred. Reopen either
  only if a diagnosed gate blocker requires it and a small measured experiment
  supplies new evidence; neither is a mandatory redevelopment project.
- Allowed implementation areas: `crates/pdfdelta-core`, `crates/pdfdelta-cli`,
  and necessary fixture/evaluation changes in `crates/pdfdelta-bench` and
  `benchmark/realworld/followup`. Reuse existing benchmark scripts rather than
  building a second general measurement framework.
- Store PDFs, rasters, raw JSON/logs and binaries under ignored
  `benchmark/realworld/cache/followup-*` or `/tmp`; retain minimal replay material,
  hashes, source references and summaries in the repository.
- Public primary-source downloads are allowed. Existing local read-only inspection
  and rendering tools are allowed. No push, PR, external issue or messaging action.
- Commit evaluable units with Conventional Commits and required checks. Keep
  comments and repository documentation in English; communicate in Japanese.
- Preserve `IMPLEMENTATION_PLAN.md`; use the new root `PLAN.md` for this run.

## Verification commands and result contracts

The small `benchmark/realworld/followup/verify.py` checker below is a planned
deliverable in C1, not an existing command. It must reuse current capture/scoring
data, validate hashes and denominators, and print every gate with observed and
required values. It must fail if required evidence is absent. Do not replace it
with a manually edited `all_passed` flag.

```sh
PYTHON_UV=0 python benchmark/realworld/followup/verify.py --stage registration
PYTHON_UV=0 python benchmark/realworld/followup/verify.py --stage diagnosis
cargo test -p pdfdelta-core --test document_text_scopes_fixture
cargo test -p pdfdelta-core --test document_groups_fixture
PYTHON_UV=0 python benchmark/realworld/followup/verify.py --stage development
PYTHON_UV=0 python benchmark/realworld/followup/verify.py --stage blind-freeze
PYTHON_UV=0 python benchmark/realworld/followup/verify.py --stage blind
PYTHON_UV=0 python benchmark/realworld/followup/verify.py --stage final
```

Every stage exits 0 only when its required evidence passes; absent evidence or
an unmet gate exits nonzero and prints the reason. Test missing/stale evidence,
duplicate pair counting, inferred-only recovery, oversized B context, empty-source
completion and a genuine all-gates-pass example. Synthetic checker self-tests
never count as document recovery.

Reuse `capture-comparisons.py` for native, common text and all-channel records,
`pdfbench validate-literal-selectors` for source resolution, and the existing
layout-control scorer. Existing scripts may receive minimal versioned adapters
for the new manifest, while old results and scoring semantics remain immutable.

G3 counts completed pairs, not processes or scopes. Require the full common-text
coverage and search contract plus expected process exit 0/1. A local complete
flag, empty candidate universe, a timeout, an extraction failure, a changed
channel set, or a raised resource limit cannot satisfy it. Run the fixed completion
panel twice per revision with stable flags to check that completion is repeatable.
Record process wall time, peak RSS, internal budget stops and report bytes for
every attempt. Do not relabel peak RSS as maximum live allocator memory; collect
bounded live-allocation telemetry only if making an allocator-memory claim.

All workspace changes must pass:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The checked CI workflow calls `mise run ci`, which additionally runs:

```sh
cargo run -p pdfdelta-bench --locked -- verify
```

Run generated-fixture verification when implementation/fixture changes warrant
it and at final integration. Do not repeatedly run an unchanged whole corpus.

## Checkpoints

- [x] C0: Reconcile the earlier completion claim with actual evidence; record the
  untouched baseline, new thresholds and exposed-set status. Verify:
  `git status --short` and `git rev-parse HEAD`; retain `baseline.json`.
- [ ] C1: Freeze the development/completion panel, source targets, controls and
  limits; implement the minimal fail-closed checker and capture missing baseline
  observations. Verify: `verify.py --stage registration` returns 0 and the final
  checker remains nonzero because recovery/completion evidence is still missing.
- [ ] C2: Assign the earliest demonstrated blockers to every fixed target; extract
  the smallest counterexamples without discarding source uncertainty. Verify:
  `verify.py --stage diagnosis`; every unresolved target has evidence and a next
  action, not merely a proposed algorithm.
- [ ] C3: Fix one shared acquisition/search/comparison cause per iteration and
  establish the proof obligation with a regression and affected real-pair replay.
  Verify: the relevant targeted test (including the two fixture commands above
  for scope work), retained pair score and progress-log evidence. Repeat as needed.
- [ ] C4: Meet G1 and G3 on the preregistered panel, preserve exact controls, and
  record A/B range judgments, independent content/layout axes and all route costs.
  Verify: `verify.py --stage development` returns 0. Do not begin blind selection
  while these gates fail.
- [ ] C5: Freeze production tree, executable hashes, lockfile and comparison limits;
  select 12 new series across six families and fix source annotations before any
  comparison output. Retain failures/replacements and exclude every exposed series.
  Verify: `verify.py --stage blind-freeze` returns 0.
- [ ] C6: Run the frozen baseline/current blind comparison and adjudicate G2/G4.
  Verify: `verify.py --stage blind` returns 0. If implementation/configuration must
  change after exposure, promote the set to development and return to C3/C5;
  never patch the frozen blind gold or quietly replace an inconvenient pair.
- [ ] C7: Export source-reachable review bundles for the counted new recovery,
  finish per-family results and reproduce all gates with the mandatory checks.
  Verify: `verify.py --stage final` and all quality commands return 0. Commit the
  evidence and report remaining limitations separately from unmet requirements.

## Iteration and append-only progress log

After each attempt record: timestamp, checkpoint, hypothesis, changed files/commit,
targeted command, result and artifact hash/path, elapsed/remaining time, and the
next evidence-driven action. Update checkboxes without rewriting earlier log rows.
Keep the detailed per-pair evidence in `benchmark/realworld/followup`; the plan log
is a compact index, not a raw transcript.

| Date | Checkpoint | Hypothesis/change | Evidence | Next |
| --- | --- | --- | --- | --- |
| Pending execution | Planning | User selected G1/G2, common-text G3 and a 12-hour cap | Planning dialogue; baseline a2904f2 | Confirm plan, then start C0 |
| 2026-09-11 | Planning approved | User approved the contract and complete plan | User confirmation: OK | Await explicit goal execution; then start C0 |
| 2026-09-11 07:28:43 UTC | C0 complete; C1 panel fixed | Preserve a2904f2 baseline; promote exposed blind set to development; select all 36 active natural pairs without dropping failures | baseline.json and panel.json; 72 input hashes verified; frozen executable hash verified | Freeze follow-up source targets and implement the fail-closed checker; elapsed <5 min, remaining >11 h 55 min |


## Risks, blocked conditions and stopping

- Twelve hours is a hard bound for one goal execution, starting when execution
  begins, not when this plan is drafted. Do not silently renew it. Stop/report at
  the cap even if work remains; a time cap is not successful completion or itself
  evidence of an external blocker. Do not start a blind round that cannot reasonably
  finish and be scored in the remaining time.
- A blocked task needs a missing external prerequisite, an unavailable required
  source, or a demonstrated need to violate an invariant/accepted constraint after
  applicable safe alternatives are exhausted. Difficulty or one unsuccessful fix
  is not a blocker. Report attempted paths, evidence, unmet gates, the exact missing
  prerequisite and the next user decision. Follow the platform's blocked-state
  recurrence rule before marking a tracked goal blocked.
- More than two consecutive iterations without reducing a demonstrated blocker
  triggers a diagnostic reassessment, not weaker thresholds or automatic success.
- Fresh blind selection/acquisition can fail or exhaust time. Retain those attempts
  and report the missing G2 evidence. Never reuse an exposed series as blind.
- Same-page B closure may not extend safely across columns/pages; all-text complete
  may require exact ownership evidence that B deliberately lacks. If no valid
  source proof exists, retain B/C/unresolved rather than merging B into coverage.
- Native report sizes above 10 GB are a measured cost. Bound experiment output and
  retain failed/partial captures. Deduplication is permissible only with versioned,
  lossless source reconstruction and unchanged result meaning, not evidence removal.
- Finish only after all G1-G5 pass. Otherwise leave the goal unmet and report
  achieved deltas, failed conditions, regressions and remaining work. Do not call
  implementation or evaluation completion fulfillment of the objective.

## Goal contract

```text
/goal Demonstrate additional source-backed ID-free prose recovery on fixed references in at least 6 natural PDF revision pairs from 3 independent publishers/producers and, after freeze, at least 3 pairs from 2 independent publishers/producers in a fresh 12-pair blind set; also make at least 2 additional preregistered pairs document-wide complete on the common --channels text route under unchanged budgets, with no losses of previously complete panel pairs. Follow /home/hayato/ghq/github.com/hayatosc/pdfdelta/PLAN.md and update its append-only progress log after each attempt. Preserve A/B/C separation, exact masks, source provenance, resource limits, regression contracts and all five acceptance cases; retain P2/P6 measured deferrals. Verify every gate with PYTHON_UV=0 python benchmark/realworld/followup/verify.py --stage final, the required workspace checks and generated-fixture verification. Work only in the declared repository/evaluation/cache boundaries, commit verified units, and do not publish externally. Stop after 12 hours from execution start or a demonstrated external/constraint blocker; report unmet gates and evidence without marking the goal complete unless every gate passes.
```
| 2026-09-11 07:39:48 UTC | C1 in progress | Registered all 36 historical source references; 28 prose candidates, 27 source-resolved, 13 independent producers; finite B extents prohibit unselected context. Production unchanged. | `followup/targets.json` SHA-256 `a1ec08f034a00315acd9fc7e12c916214290511329ff3a49396dc9bdded78e7a`; four checker failure-mode tests pass; `verify.py --stage final` exits 1 for absent baseline index; common-text baseline repeat running in ignored cache. About 11 minutes elapsed, 11h49m remaining. | Finish 72 baseline observations, source controls registration and fail-closed stage checks before C1 completion. |
| 2026-09-11 07:42:57 UTC | C1 registration unit | Bound both common-text attempts for all 36 pairs; all remain incomplete, including explicit W-4 process failures. Registered 60 generated and 3 real-source controls. | `baseline-observations.json` SHA-256 `3fd5e354615d5fe46711ecd117d61432f4a02722f402056d6866c58461a044c7`; registration exits 0; final exits 1 for absent later-stage evidence. fmt/clippy pass; workspace tests 2326 passed, 2 ignored. Four checker tests pass. Elapsed 862s, remaining 42338s. | Complete source-bound gate evaluator/self-tests and baseline target scoring; continue earliest-blocker diagnosis. C1 remains open until its complete checker contract is implemented. |
| 2026-09-11 08:07:35 UTC | Constraint stop; goal not achieved | All 72 frozen-worker probes completed: 35 pairs have non-text paint on at least one side; remaining NIST controls pair failed both response-bound probes. Preserving native Text inventory and excluding OCR leaves at most one eligible complete pair, below G3 gain of two. No production changes or new blind selection. | `constraint-stop.json` SHA-256 `52f47b3395a23270af4f863f664f5e298d20c763f4d5338d41abadb26016d9f5`; `RESULTS.md` lists unmet G1/G2/G3/C1/C2/G5. Registration exits 0; final exits 1 and prints all gates. fmt/clippy pass; tests 2326 passed, 2 ignored; generated verification 48/48 passed (strict author-intent 42/48); six checker tests pass. Elapsed 2340s, unused time 40860s. | Stop under the approved constraint condition. A revised inventory/proof contract or evaluation scope is required before this fixed G3 claim can be resumed; do not mark this goal complete. |
