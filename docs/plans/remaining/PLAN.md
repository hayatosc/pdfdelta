# Complete the remaining source-backed recovery gates

Status: active; no gate achievement claimed.
Execution started 2026-09-11 14:23:15 UTC; hard stop 2026-09-12 02:23:15 UTC.
The user authorized this plan and its execution through the new goal contract.
This Markdown is the execution source of truth; [PLAN.html](PLAN.html) explains it.
Historical root plans and `benchmark/realworld/followup` records remain immutable.

## What carries forward and what must change

Reuse the frozen 36-pair panel, 72 common-text baseline captures, 72 native inventory
probes, original source annotations, finite target extents and controls from
`benchmark/realworld/followup`. Bind references by SHA-256 in the new registration.
The panel has 28 body candidates, 27 source-resolved candidates and 13 canonical
producers. Baseline common-text B range hits are zero; document-wide completion
is 0/36 in both attempts. Do not shrink this denominator.

The earlier stop establishes a limitation of the existing inventory proof rule,
not the impossibility of all additional non-OCR source evidence. Test new proof
methods before declaring a constraint blocker. Do not remove non-text uncertainty
or change completion semantics merely to increase G3. Local B recovery alone does
not establish document-wide completion. Preserve native acquisition defaults
(35 seconds, 128 MiB response bound) and common-text capture timeout 180 seconds,
budget scale 1. Retain acquisition failures as failures.

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

- Working baseline: `31adc6a`; production source
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
  `benchmark/realworld/remaining`. Reuse existing benchmark scripts rather than
  building a second general measurement framework.
- Store PDFs, rasters, raw JSON/logs and binaries under ignored
  `benchmark/realworld/cache/remaining-*` or `/tmp`; retain minimal replay material,
  hashes, source references and summaries in the repository.
- Public primary-source downloads are allowed. Existing local read-only inspection
  and rendering tools are allowed. No push, PR, external issue or messaging action.
- Commit evaluable units with Conventional Commits and required checks. Keep
  comments and repository documentation in English; communicate in Japanese.
- Preserve `IMPLEMENTATION_PLAN.md`; preserve root `PLAN.md` as historical evidence; use this plan for this run.

## Verification commands and result contracts

The `benchmark/realworld/remaining/verify.py` checker below is a planned
deliverable in C1, not an existing command. It must reuse current capture/scoring
data, validate hashes and denominators, and print every gate with observed and
required values. It must fail if required evidence is absent. Do not replace it
with a manually edited `all_passed` flag.

```sh
PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage registration
PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage diagnosis
cargo test -p pdfdelta-core --test document_text_scopes_fixture
cargo test -p pdfdelta-core --test document_groups_fixture
PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage development
PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage blind-freeze
PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage blind
PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage final
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

## Checkpoints and verification

- [x] C1: Bind the unchanged panel, targets, controls and baseline; finish a fail-closed
  evaluator for every gate, including a genuine synthetic all-pass path and tests
  for missing/stale evidence, duplicate pairs, C-only recovery, oversized B ranges
  and false empty completion. Synthetic fixtures never count toward G1/G2/G3.
  Verify: `PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage registration`
  and checker self-tests; final remains nonzero until real evidence exists.
- [x] C2: Record the earliest demonstrated blocker for every frozen target, with a
  source-linked observation and the smallest counterexample for each shared cause.
  Verify: `PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage diagnosis`.
- [ ] C3: Experiment with additional non-OCR source proofs for whole-text inventory
  and local closure. Require positive and adversarial fixtures and affected real-PDF
  replay. Absence of an existing feature alone is not a blocker. Never treat nearby
  paint geometry or optimizer necessity as source identity.
  Verify: `cargo test -p pdfdelta-core --test document_text_scopes_fixture` plus the
  relevant acquisition/proof regression and retained experiment results.
- [ ] C4: Fix demonstrated shared causes one at a time. Meet G1 (6 pairs/3 producers)
  and G3 (gain >=2, no losses), with two fixed-panel repeats and unchanged budgets.
  Verify: `PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage development`.
- [ ] C5: Freeze production tree, executable, lockfile and configuration, then acquire
  12 previously unexposed revision series spanning six families, English/Japanese
  and at least three producers. Resolve independent source annotations before
  viewing comparison results. Preserve failed acquisitions/replacements.
  Verify: `PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage blind-freeze`.
- [ ] C6: Demonstrate G2 (3 pairs/2 producers) on that frozen set; adjudicate every
  new A/B output, including out-of-target findings. If tuning after exposure is
  necessary, promote the set to development and select a fresh 12 after a new freeze.
  Verify: `PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage blind`.
- [ ] C7: Retain source-reachable review bundles, separate A/B/C counts, per-family
  precision/recall (undefined where gold is unavailable), completeness denominators,
  costs, all 60 generated and three real-source controls, and all five acceptance
  cases: wrap invariance, page-break invariance, exact single replacement, insertion,
  and deletion. Run all quality commands above and generated-fixture verification.
  Verify: `PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage final`.

## Iteration log and stopping

Append timestamp, hypothesis, changed files/commit, command, result, artifact path/hash,
elapsed/remaining time and next action after each attempt. Commit verified units with
Conventional Commits. No external publication. More than two unsuccessful iterations
on a cause trigger reassessment, never weaker acceptance thresholds.

Stop at the 12-hour deadline or a demonstrated external/constraint blocker after
applicable safe alternatives are exhausted. Difficulty, incomplete implementation,
or the old native-rule upper bound alone is not sufficient. Report attempted paths,
missing prerequisites, achieved deltas, regressions and unmet gates. Follow the
platform recurrence rule before marking the tracked goal blocked. The goal is
complete only when every G1-G5 gate and required verification succeeds.

| Time (UTC) | Checkpoint | Attempt and evidence | Next |
| --- | --- | --- | --- |
| 2026-09-11 14:23:15 | Planning approved / C1 | New goal authorizes this plan and execution; clean HEAD 31adc6a; preserve old results and test additional source proof. | Bind historical evidence and finish the evaluator. |

| 2026-09-11 14:34:20 UTC | C1 registration unit | Fixed historical hashes; registration passes for 36 pairs, 27 resolved body candidates and 13 producers; common-text baseline 0/36 twice. Added independent gate reduction and complete-search checks (2 tests pass). Final source-adjudication/freeze adapters are still unfinished; C1 remains open. | All four workspace/generated checks pass; logs in `cache/remaining-registration-checks/checks.json`. actrun could not create a sandbox worktree, so the same CI commands ran directly. Continue evaluator and new-proof diagnosis. |

| 2026-09-11 15:07:08 UTC | C3 acquisition experiment 1 | Bounded paint probe: 72 attempts, 63 successful documents / 30 complete pairs; 9 explicit diagnostic-limit failures. Conservative non-text projection and non-font resource closure: no entire pair matches. | `remaining/paint-probe.json`; preserve frozen v1 source/executable in cache; no completion claim. |
| 2026-09-11 15:07:08 UTC | C3 acquisition experiment 2 | Marker-free projection on six candidate pairs: W-2 matches all 11 pages, including all 7 painted pages. The other five pairs do not match. This omits possible optional-content state and remains diagnostic. | `remaining/paint-marker-probe.json`; investigate a complete execution certificate before any G3 use. |
| 2026-09-11 15:07:08 UTC | C3 acquisition experiment 3 | Schedule C/SE projected traces contain actual line-width, length and position differences. Eight selected-page dumps distinguish this from marker-only changes. | `remaining/paint-state-probe.json`; do not use near-geometry matching as text identity. |
| 2026-09-11 15:07:08 UTC | C1/C2 local-source investigation | Cached tuple-form glyph metadata locates target extents and controls relative to last paint. Earlier paint can still overlap the visual band, so render order alone does not close it. Range scoring now checks new A/B adjudications, rejects C/oversized ranges, and keeps absent strict gold undefined (3 tests pass). | `remaining/local-order-probe.json`; C1 final adapters and per-target earliest-blocker diagnosis remain open. Elapsed 2633s, remaining 40567s. Test conservative local paint bounds next; production is unchanged. |

| 2026-09-11 15:09:13 UTC | Verified experiment unit | Paint probe test, three evaluator tests, fmt, workspace clippy/tests and generated-fixture verification pass. Final remains nonzero for missing diagnosis and later evidence; no production changes or new blind selection. | `cache/remaining-probe-checks/checks.json`; commit diagnostic evidence, then implement/test local paint-bound acquisition. |

| 2026-09-11 15:33:50 UTC | C3/C4 bounded native paint | Added conservative outward-rounded bounds for images, straight fills and single-segment strokes; unknown paint stays explicit. Local native B intervals require the full boundary-ink band to exclude every paint, an exact native glyph inventory and no dependent text issues. Whole-page text completeness and strict source ownership are unchanged; cache format is 6, glyph identity profile remains unchanged. | Local positive/negative fixtures pass; fmt, clippy, workspace tests and generated verification pass (48/48). Logs: `cache/remaining-paint-bounds-checks/checks-2.json`. Initial clippy findings (Option idiom and test panic messages) were fixed. No real-PDF gains claimed; measure the fixed panel next. |

| 2026-09-11 15:58:57 UTC | C3 full-panel marker diagnostic | Combined retained six-pair v2 probe with the other 30 pairs. Thirty pairs have both diagnostic reports; six have explicit parser/resource failures. Only W-2 has equal full projected page multisets. This still omits execution dependencies and is not a completion proof. | `remaining/paint-marker-panel-probe.json`; no G3 gain. |
| 2026-09-11 15:58:57 UTC | C4 bounded-paint pilot | Committed production 08e4e96, one common-text observation for each of 36 fixed pairs: 35 reports, one explicit ownership-limit failure; zero target B range hits, zero strict control-source claims, zero complete pairs. Additional out-of-target reviews include citation-number changes and require adjudication. | `remaining/bounded-paint-pilot.json`; raw captures in `cache/remaining-bounded-paint`. No repeatability or recovery claim. |
| 2026-09-11 15:58:57 UTC | C2/C4 boundary-padding experiment | Source exports show identical EDPB boundary text except for an added final U+0020. Added an exact unpadded native-text feature below whole-literal matching in the objective. It is admitted only as a non-owning boundary; the entire padded paragraph stays uncompared, including all edge spaces. Interior whitespace differences, duplicate bodies and forged premises are rejected. | Initial prototype preserved full-paragraph masks but was narrowed before measurement to avoid treating boundary evidence as whole-paragraph identity. Seven local scope tests pass, including zero strict coverage from padded boundaries. Final fmt, clippy, workspace tests and generated verification (48/48) pass; `cache/remaining-padding-checks/checks-2.json`. Measure fixed inputs next. |

## Goal contract

Follow this plan to demonstrate additional A/B recovery on fixed body targets in
6 pairs/3 producers, on a newly frozen blind set of 12 pairs in 3 pairs/2 producers,
and document-wide common-text completion gains in at least two fixed-panel pairs
with no losses, using unchanged inputs, budgets and completion contracts. Preserve
source evidence, A/B/C separation, exact masks and the five acceptance cases. Verify
all gates with `benchmark/realworld/remaining/verify.py --stage final`, mandatory
workspace checks and generated-fixture verification. Append attempts and commit
verified units. Stop at 12 hours or a proven constraint blocker, report unmet gates,
and never mark success before all gates pass. Do not publish externally.
| 2026-09-11 16:18 UTC | C2 padding-boundary panel pilot | All 36 attempts retained: zero target B hits, zero complete pairs, zero strict control atoms claimed. | `remaining/padding-boundary-pilot.json`; no gate gain. |
| 2026-09-11 16:18 UTC | C2 source-paired quoted-word wrapping | The earliest remaining controller target blocker was two ambiguous breaks before paired directional quoted words. A source-only linear scan resolves breaks before a single paired Latin word; unmatched, ASCII, reversed and multiword cases remain unresolved. | Normalization regression, workspace format/lint/tests and 48 generated cases pass. `cache/remaining-quoted-checks/checks.json`. |
| 2026-09-11 16:18 UTC | C2 quoted-word target pilot | One fixed B source-range hit covers exactly 986 source atoms; no whole-document completion. The interval contains the added non-negotiability sentence. Whitespace-only out-of-target reviews require further source adjudication. | `remaining/quoted-boundary-pilot.json`; not yet counted as a G1 recovery. |
| 2026-09-11 16:25 UTC | C2 native spacing restraint | A source review found two intervals differing only in reconstructed/explicit ASCII spaces. Native B now declines space-only differences without changing canonical text, conditional masks or strict coverage. The fixed 986-source target hit remains; six retained B outputs have source-content and boundary rationales, including five out-of-target findings. | `remaining/native-spacing-pilot.json`; regression, workspace checks and 48/48 generated cases pass in `cache/remaining-native-spacing-checks`. Single-pass only; no final gate claim. |

### 2026-09-11 16:38 UTC — bind stage records and reject evidence substitution

- Implemented development/blind/final evidence adapters and 189-attempt control
  scoring. Captures require distinct attempts, unchanged input hashes, executable,
  budgets, channels and retained process costs. Known strict gold cannot be
  overridden by a manual source adjudication. Native tentative candidates and
  unresolved changed regions remain non-recovery observations under C.
- Added tests for stale/missing reports, changed budgets, reused capture attempts,
  strict-gold violations and native non-recovery outputs. Existing finite-range,
  inferred-only, duplicate-pair and empty-completion tests remain in place.
- Retained `native-spacing-panel-pilot.json`: one single-pass target B range hit
  (EDPB controller/processor), zero whole-document completions among all 36
  attempts; one resource failure remains an explicit failure. No gate is claimed.
- Final verification currently rejects the missing `diagnosis.json`. C1 remains
  open pending a genuine all-stage synthetic pass test; C2 requires source-linked
  earliest-blocker diagnosis for every target, not just corpus-level counters.
- Elapsed 2h15m; remaining 9h45m. Next: complete target-specific diagnostics and
  continue shared-root fixes under the unchanged target and completion contracts.

### 2026-09-11 16:53 UTC — exercise the complete evidence path

- Added a constructed 36-pair development / 12-pair blind / 189-control evidence
  tree with a temporary Git registration commit. All six stages execute their
  actual parsers and gate checks; only filesystem roots are redirected. No gate
  function or result is mocked. Source-review mutation and removal make final
  fail. The fixture is evaluator-only and never contributes to real-PDF counts.
- Seven checker tests pass. Strict-gold subsets/duplicate outputs cannot pass as
  exact events; unhandled inferred outputs now fail instead of disappearing from
  C metrics. Blind comparison timestamps must follow annotation completion.
- C1 is complete. Production is unchanged. Mandatory format, Clippy and workspace
  checks pass; actual final still fails while real diagnosis/development/blind
  records are absent.
- Bounded source-graph exports for the remaining eligible targets are retained
  under `cache/remaining-target-diagnosis`; these are diagnostic observations,
  not additional repeated benchmark claims. Fixed EDPB body extents exclude
  separately drawn paragraph labels; the SSDF extent excludes a trailing space.
  These source mismatches cannot be removed by widening frozen extents.
- Elapsed 2h30m; remaining 9h30m. Next: source-linked target diagnosis and the
  smallest shared proof or acquisition change supported by those observations.

### 2026-09-11 17:23 UTC — localize verification stops and reject page-break B

- C2 now records all 36 frozen targets with hash-bound source evidence and
  minimal counterexample references. Observed first missing premises: 15 scope,
  six normalization, six acquisition; nine reporting cases cover eight non-body
  targets and the recovered controller/processor pilot. Diagnosis verification
  passes. Missing native sources are never an empty successful comparison.
- Exact-key buckets now preserve every omitted endpoint when token verification
  exhausts its budget. Previously this late stop discarded the known dependency
  bounds and suppressed independent comparisons. Key-index exhaustion still
  retains an unknown population; partial repeated buckets cannot manufacture
  uniqueness. The full candidate universe, weights and budgets are unchanged.
- GPT-3 local comparisons increase from 0 to 1547, covering 121901 native source
  atoms per side. An exposed B was a known false page-break truncation: its exact
  continuation remained on the next page. A bounded native external-continuation
  check declines this B without asserting a move or adding coverage.
- `bucket-locality-pilot.json` retains the before/unsafe-probe/corrected reports
  and source diagnosis. `locality-panel-pilot.json` retains all 36 attempts: 35
  reports, one resource failure, one fixed target B hit, zero document completions,
  88 B outputs and zero fixed strict-control atoms claimed. No new gate passes.
- Required format, Clippy and workspace tests pass (2337 passed, two ignored);
  generated fixtures pass 48/48; seven evaluator tests pass. Clippy initially
  rejected one test unwrap, replaced with a contextual expect. A rebuild proved
  executable byte identity after that test-only edit; no corpus rerun was needed.
- Elapsed 3h; remaining 9h. Next: a reversible layout fix for measured narrow
  column gutters. Target text gaps stay below 0.83 font sizes, while observed
  cross-column gaps are 2.26–3.12; the current four-font-size ceiling merges them.

### 2026-09-11 17:35 UTC — separate measured narrow column gutters

- Reduced the default inline line-joining gap from four to two font sizes.
  A scale-invariant fixture rejects the measured 2.258/3.118-font-size gutters
  while preserving a 1.5-font-size word gap and every glyph assignment.
- `column-gap-pilot.json` binds three same-budget captures and BERT source
  graphs. The BERT target now occupies nine source-exclusive fragments on each
  side (343/360 glyphs, no adjacent-column glyphs). Reading-order links still
  fail to connect these fragments; all three fixed-target B counts remain zero.
  No document completion or recovery gate gain is claimed.
- Format, Clippy and workspace tests pass (2338 passed, two ignored); generated
  fixtures pass 48/48. Elapsed 3h12m; remaining 8h48m. Next: inspect the shared
  region partition that leaves source-exclusive column lines interleaved.

### 2026-09-11 17:50 UTC — admit valid column cuts behind an isolated margin

- Whole-page BERT evidence reveals a narrower 1.559-font-size body gutter.
  The line ceiling is now 1.5; the fixture still retains a 1.5-font-size word gap.
  Column partitioning now selects the largest gap that leaves the required line
  count on both sides. An isolated margin previously made the largest gap
  inadmissible and hid the smaller valid column gutter.
- Applying the same selection change to horizontal bands altered an existing
  staggered-column contract. That extension was removed; all 94 targeted layout
  and text-scope tests pass. Required format, Clippy and workspace tests pass
  (2339 passed, two ignored), and generated fixtures pass 48/48.
- `column-partition-panel-pilot.json` binds the rebuilt executable, all 36
  same-budget captures, and BERT source graphs. Its fixed old/new target is now
  one paragraph per side, retaining all 343/360 source glyphs and no extra glyphs.
  The preceding paragraph also changes, so two unchanged boundaries remain absent.
  The panel yields 36 reports, 90 B outputs, the existing one controller/processor
  target hit, zero document completions, and zero strict-control atoms claimed.
  New out-of-target reviews are not adjudicated gate recovery.
- After two gutter iterations, reassess rather than further lower thresholds.
  A direct NIST control-catalog worker probe exits at the unchanged 128 MiB
  response ceiling in 3.10 seconds, still serializing native glyphs. Next: test
  lossless transport redundancy removal at unchanged time/byte/item limits.
  Elapsed 3h27m; remaining 8h33m.

### 2026-09-11 17:57 UTC — retain repeated worker context once

- Private worker transport now reuses exactly equal consecutive glyph context:
  page, vertical bounds/baseline, direction, font, rendering/clip state and source
  object. Floating context values use their full bit patterns, including signed
  zero. Text, raw codes, horizontal geometry, paint order and operator provenance
  remain per glyph. A missing initial context and unknown versions fail decoding.
- The expanded round-trip fixture retains mapped/unmapped glyphs, every render
  mode, signed zero and auxiliary evidence. The previous transport failed its
  response-size assertion; the new encoding fits below one third of named JSON.
  No worker, parser, extraction or evidence ceiling changes. Required format,
  Clippy and workspace tests pass (2339 passed, two ignored); generated 48/48 pass.
- `worker-context-pilot.json` binds two comparison attempts and direct worker
  probes. NIST risk-management revision 2 now retains 643987 new-side sources,
  previously zero. Candidate indexing still suppresses local comparison. The
  larger controls catalog advances from about 703000 to 1310000 serialized glyphs
  before the same 128 MiB ceiling, but still cannot return a complete acquisition.
  No fixed-target A/B or document-completion gain is claimed.
- Elapsed 3h34m; remaining 8h26m. Next: use the recovered source evidence to
  identify duplicate indexing work, and measure remaining wire-size components
  before choosing another transport change.

### 2026-09-11 18:09 UTC — share unchanged literal and boundary hashes

- Unpadded native text previously hashed the same token slice twice for literal
  and padding-boundary keys. Reusing that fingerprint preserves both key domains,
  collision verification, candidate population and weights. The bounded fixture
  fails before the change and now matches the complete high-budget population.
- `shared-key-pilot.json` retains four same-budget captures. Index work falls
  from 581795 to 489892 for controller/processor, 179874 to 105323 for BERT, and
  1000000 to 680186 for GPT-3. Their compared-source counts and fixed-target B
  results are unchanged. The recovered NIST risk-management source population
  still exceeds its key-index budget. No G1/G3 gain is claimed.
- Required format, Clippy and workspace tests pass (2340 passed, two ignored);
  generated fixtures pass 48/48. Elapsed 3h46m; remaining 8h14m. Next: investigate
  source-local closure for the fixed Mask R-CNN abstract, whose page retains an
  unsupported nested form invocation and disconnected column fragments.

### 2026-09-11 18:37 UTC: recover columns behind terminal lines and headers

- The fixed Mask R-CNN page has a centered terminal line splitting an otherwise
  usable column gutter. When ordinary cuts fail, isolate a supported terminal
  line only if its width is at most twice the median line height and all other
  lines are separated by at least that distance. Every line remains present.
- A smaller header gap may be used only when its lower partition exposes an
  ordinary uninterrupted column gutter. The 2.5-median-height floor remains;
  ordinary cut priorities and default ratios are unchanged. A global horizontal
  ratio reduction from 0.05 to 0.04 was rejected after failing the real fixture.
- `header-columns-pilot.json` binds the build, two same-budget RCNN captures,
  source graphs, rejected attempt and check logs. The fixed Mask target goes
  from 12 old and 14 new source-exclusive fragments to two old and three new
  consecutive fragments. Both target recovery counts and complete counts remain
  zero. The rotated sidebar still separates the new abstract heading from its
  body; the unsupported figure invocation still prevents local paint closure.
- Required format, Clippy and workspace tests pass (2341 passed, two ignored);
  generated fixtures pass 48/48. The scale regression fails before the change
  and passes after it, including wide and insufficiently separated terminals.
  Next: retain the sidebar's uncertainty while recovering independent body order,
  then investigate the failed Form's explicit bounding box.

### 2026-09-11 18:53 UTC: retain failed Form invocation bounds

- Failed Form invocations retain an outward-rounded bound derived from their
  explicit clipping box and the complete Form/caller/page transform. Missing,
  reversed, non-finite or unresolvable bounds remain opaque. Partial paints are
  rolled back with the failed invocation's other effects, then represented by
  one opaque operation with caller stream/operator provenance.
- A checked paint-index reference links the retained extraction gap to that
  bound. Only disjoint local native bands may use it for non-owning B review;
  the extraction issue and incomplete page/document inventories remain. Invalid
  references and touching paint do not establish closure. Cache version 7 and
  the updated backend profile prevent reuse of older extraction results.
- `form-bounds-pilot.json` binds the final build and two fixed RCNN captures.
  The Mask figure occupies the right column while its abstract occupies the
  left. Both fixed target hits and pair completion counts remain zero; one
  extra out-of-target B output needs adjudication and is not counted.
- A separate margin-isolation attempt was rejected: it recovered the new
  abstract's leading adjacency but exposed a horizontal split that lost the
  trailing adjacency. Its patch and source graphs are retained. No margin
  exception is present in the committed layout algorithm.
- Format, Clippy and workspace tests pass (2343 passed, two ignored), including
  transformed Form bounds, opaque fallback, invalid-reference and local-closure
  tests. Generated fixtures pass 48/48. Next: investigate first-line paragraph
  fragmentation and recover source-closed discovery paths across layout gaps.

### 2026-09-11 19:00 UTC: retain justified first-line paragraph continuity

- The Mask abstract's first-line indent is 1.39 median line heights; its right
  edge differs from the next full line by only 0.00012 native units. A bounded
  outdent exception now requires at most 1.5 heights, at least 80% of the next
  line's width and right-edge agreement within 0.1 heights. The ordinary indent
  ceiling, vertical-gap limit, font checks and grid boundaries remain intact.
- `first-indent-pilot.json` retains five same-budget captures and source graphs.
  The Mask target becomes one old paragraph (923 glyphs) and two consecutive new
  fragments (984 glyphs), without additional context. The existing EDPB target
  remains recovered; the other four targets and all complete counts remain zero.
- The regression fails before the change and passes at three scales, rejecting
  short labels and larger outdents and keeping the next paragraph separate.
  Format, Clippy and workspace checks pass (2344 passed, two ignored), and all
  48 generated fixtures pass. Next: source-closed discovery across the remaining
  new abstract heading/body adjacency gap, without changing strict ownership.

### 2026-09-12 11:38 UTC: deadline stop, objective not achieved

- The recorded deadline was 2026-09-12 02:23:15 UTC. The last earlier clock
  observation was 2026-09-11 18:59:49 UTC; the next recorded observation was
  2026-09-12 11:38:11 UTC. The time limit was exceeded before detection. The
  cause of this observation gap is not established, and an on-time stop is
  not claimed. New experiments stopped when the exceeded deadline was detected.
- The unfinished native adjacency-discovery experiment was saved as a patch
  and removed from the working tree. Production remains the verified b11ef89
  revision. Its small regression passed, but it had neither completed all
  quality gates nor a real-PDF evaluation at the stop; no gain is claimed.
- `remaining/stop.json` retains the stop contract and final verifier output.
  `verify.py --stage final` exits 1 because `remaining/development.json` is
  missing. Formal G1 and G2 counts are zero, no G3 completion gain is proved,
  and G4/G5 are unfulfilled because the final phase evidence is incomplete.
  The one EDPB pilot hit is not a substitute for repeated adjudicated G1 data.
- The retained production revision passed format, Clippy, 2344 workspace tests
  (two ignored) and 48/48 generated fixtures. This protects the verified units;
  it does not establish the missing six-pair recovery, new blind evaluation or
  whole-document completion goals. No external publication was performed.
