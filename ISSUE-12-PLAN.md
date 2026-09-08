# Local content comparison with uncertain reading order

Status: execution completed with a failed focused acceptance gate,
2026-09-08. The [execution report](benchmark/realworld/results/2026-09-08-issue12-local-comparison/execution-report.md)
records the frozen results and remaining failures. Issue #12 remains open;
acceptance expectations are unchanged. This execution used no subagents.

| Step | Execution result |
| --- | --- |
| Selected-miss diagnosis | Completed for the two CSF sentences and FIPS DSA note; original source reductions and baseline evidence retained. |
| Interval correspondence | Implemented with split/merge and competing-interval controls; five fixed source-glyph edits recover, while the original three targets remain unrecovered. |
| One-sided edits | Shared parent localization emits exact insertion/deletion ranges and passes reversal checks. |
| Independence and limits | Focused transformations and 2,065 workspace tests pass; the strict generated matrix remains 42/48. |
| Five-pair evaluation | Coverage improves on all five pairs. Four pairs match 0/17 annotations; SP800's eight event expectations are unmeasured because scope coordinates are indeterminate. Its token metric has two false positives. The broader run is deferred. |
| Boundary proposal | Prepared separately in [EDIT-BOUNDARY-CONTRACT-PROPOSAL.md](EDIT-BOUNDARY-CONTRACT-PROPOSAL.md); no policy change implemented. |

## Objective and decisions

Establish more correct, localized content changes when the overall reading
order is uncertain. Retain reliable order within source-backed ranges and
establish correspondence between revisions before computing exact changes.
Do not require unrelated ranges to have a total order. Movement still needs
evidence about relative order beyond the moved text itself.

Keep the existing evidence pipeline, neutral PDF facade, reversible source
maps, shared assessment, candidate output, exclusive token ownership, and
resource accounting. Inferred order may guide candidate generation; it does
not by itself establish a result. An exact edit script alone does not prove
that the selected old and new passages correspond.

## Starting evidence

The [current evaluation](benchmark/realworld/results/2026-09-08-3bebfeb-evaluation.md)
compares the current implementation with common assessment alone. Local
recovery adds 86,283 resolved tokens, but both methods match 0 of 40 listed
exact changes. Final candidates match 17 of 40. FIPS candidates match 6 of
7 listed changes; CSF candidates match 1 of 3. These observations distinguish
candidate availability from final acceptance; they do not establish each
change's rejection cause.

The code already contains local recovery:

| Existing path | Observed contract | Question to answer with fixtures |
| --- | --- | --- |
| `assessment.rs::discover_local_domains` | Supplies existing exact recovery proposals as anchors | Are useful local anchors absent because upstream proposal boundaries differ? |
| `assessment/views.rs::discover` | Checks exact occurrences across available views and requires trusted local runs | Which evidence or search requirement prevents a target domain from forming? |
| `assessment/views.rs::one_to_one_pairs` | Requires a one-to-one pairing of entire views | Does a harmless split into several runs reject otherwise independent, bounded correspondences? |
| `assessment/views.rs::close_domain` | One anchor closes only its equal range; multiple compatible anchors can enclose a changed gap | Are resolved-token gains predominantly isolated equal anchors? |
| `assessment.rs::domain_key` | Uses discovered local domains only for proposals with both sides present | Are one-sided insertions and deletions falling back to a wider uncertain domain? |
| `assessment.rs::finish` | Assesses ordinary proposals, then discovers local domains and retries tentative proposals | Does failure occur during domain selection, localization, or final emission? |

These are observed implementation constraints and diagnostic hypotheses,
not proof that relaxing any one of them fixes the annotated failures.

## 1. Trace selected misses through the final assessment

Start with these existing development expectations:

- CSF: `all-sector-scope-emphasized`.
- CSF: `core-expanded-from-five-to-six-functions`.
- FIPS: `dsa-legacy-verification-note`.

Retain the other FIPS content edits as follow-up cases. Use
`domain-parameter-requirement-moved` as a separate movement control, not as
the first demonstration of local content recovery.

Extend the benchmark's existing diagnostic path to join annotation
occurrences with actual final candidates and `RelationAssessment` records.
Use existing records first; add a small bounded diagnostic field only when
the decisive stage cannot otherwise be observed. Do not introduce a tracing
framework or a new user-facing diagnostic workflow.

For each selected expectation, record:

- Located old/new source occurrences, including ambiguity and missing sides.
- Candidate correspondence and source-backed run/interval membership.
- The selected parent domain, its boundaries, and the evidence closing it.
- Final relation outcome, ancestor reasons, search completeness, and any
  later event-emission rejection.
- Stage work consumption and whether a resource limit prevented a decision.

Classify failures as missing correspondence, missing domain closure,
competing correspondence, ambiguous edit location, extraction uncertainty,
incomplete search, or emission mismatch. Keep legacy recovery diagnostics
separate from final assessment evidence. Expected text and annotation IDs
remain in the benchmark; production comparison never receives them.

Reduce each relevant failure to a source-backed `Document<Glyph>` fixture,
retaining the competing occurrences and unchanged surroundings that caused
the failure. Record the source pair, hashes, pages, and source ranges. A
reduction is useful only if it retains the original blocking condition.
Use a PDF fixture as well when extraction or rendering is necessary to
reproduce that condition.

Completion: the selected cases have a concrete stage-by-stage diagnosis and
at least one reproducible content-recovery failure with fixed expected
ranges. This is diagnostic completion, not evidence that recovery improved.

## 2. Establish correspondence at the necessary interval granularity

Choose the smallest shared change justified by step 1. The leading bounded
experiment is interval correspondence within existing trusted views.

Construct a case where one old run contains two independently anchored
passages and the new document places those passages in two runs. Their
internal orders and source boundaries are known; the order between the new
runs is not required to compare each passage. Add the reversed split/merge
case and a competing, overlapping correspondence case.

If whole-view pairing is the blocker, close candidate intervals from the
existing exact anchors and enforce one-to-one ownership on those source
intervals. Multiple disjoint intervals in one run may then correspond to
different runs. Do not concatenate uncertain runs or assume that different
view IDs imply different source occurrences.

Every established interval must retain verified internal order, justified
old/new boundaries, completed checks for relevant competing occurrences,
source continuity, and exclusion of intersecting extraction barriers. A
single equal anchor continues to prove only its own range. No rule may
extend it over adjacent changed text merely because the text is nearby.

If the real failure instead lacks usable anchors, work on bounded anchor
discovery within existing source-backed views. First demonstrate the missed
anchor on the reduced fixture. Do not expand search or weaken pairing rules
when the diagnosis identifies a different stage.

Completion: a previously missed, source-backed content change is established
through the normal pipeline, including final emission and ownership, while
the competing case remains unresolved. A synthetic success alone does not
complete the real-document objective.

## 3. Route one-sided edits through an established parent domain

For an insertion or deletion contained in an established local domain,
derive the missing side's zero-width boundary from the parent alignment.
Require the boundary and resulting edit to satisfy the existing localization
contract. Reuse domain proof and final emission checks instead of inventing
a separate acceptance route for each recovery generator.

Do not infer deletion from failure to find text elsewhere, or infer an
insertion boundary from the nearest block. Repeated text, multiple possible
boundaries, missing extraction evidence, and unfinished relevant searches
remain unresolved unless independently disambiguated.

Completion: local replacement, insertion, and deletion cases emit correct
source ranges and kinds despite unrelated reading-order uncertainty. Test
old/new reversal and a nearby ambiguous occurrence. Movement does not gain
acceptance merely because its text is equal or its local domain is known.

## 4. Validate recovery and independence

Add only transformations that exercise the implemented change:

- Split or merge soft block boundaries without changing source content.
- Change line wrapping and page breaks around the same content edit.
- Interleave independent drawing operations while preserving the same
  rendered glyphs, geometry, graphics state, and extraction evidence.
- Add an ambiguous neighboring passage without invalidating an independent
  established change.
- Exhaust the optional recovery budget while retaining previously
  established ordinary results and explicit incomplete-search outcomes.

Arbitrary content-stream reordering is not assumed to preserve rendering.
Keep raw codes, text, glyph geometry, and operator/object provenance.

Use the existing shared work and output limits. Changes to limits or
performance behavior require measured need; do not fund recovery by
discarding ordinary accepted results. Validate focused core/benchmark tests
after each relevant change. Before a commit run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 5. Evaluate the five target document pairs and decide the next scope

Freeze source, options, annotations, and evaluation definitions before
comparing the revised implementation with the saved current baseline.
The comparison must measure newly established annotated changes, not only
additional candidates or equal tokens. Preserve the historical production
output separately because its accepted-change semantics differ.

Evaluate ECMA-109, FIPS-186, SP800-57, CSF, and EDPB right of access. Keep the
existing annotation scope distinctions. Add source-reviewed complete local
scopes around the selected edits, including unchanged context, before using
them to claim precision. Do not derive annotations from the new output.

Required evidence for an implementation increment:

- At least one selected real content change becomes correctly established;
  candidate-only improvement is insufficient.
- The five core acceptance cases remain exact passes: wrap and page-break
  invariance, one replacement, one paragraph insertion, and one paragraph
  deletion.
- Annotated exact-change recall and resolved-token coverage do not regress
  relative to the frozen baseline on an individual pair.
- False-positive counts and false-positive changed tokens per unchanged
  token do not increase within the fixed, completely reviewed scopes.
- Limits, unsupported extraction, unresolved results, and runtime/resource
  changes remain in the report rather than disappearing from denominators.

Run the broader existing corpus as a regression check after the focused
increment passes, not after every diagnostic change. The existing ten
holdout pairs lack change annotations and have already been inspected;
their reruns cannot establish fresh blind precision or recall. A claim of
generalization requires an independently annotated, unused document family
and frozen implementation/options before that evaluation.

An increment passing these checks does not automatically close Issue #12.
Track every remaining expected-change failure across all five target pairs,
including movement. Renaming an unresolved reason is not a detection gain.

## Separate edit-boundary contract item

The existing generated matrix has 42 of 48 strict passes; six cases remain
candidates because equivalent adjacent-space edits admit different
boundaries. Keep this release failure visible. It does not prevent work on
unambiguous local correspondence and content edits.

Prepare a separate concrete proposal for a deterministic boundary
representation inside an already established domain. Any proposal must
preserve whitespace content, source mappings, and reconstruction of the
exact old/new strings; it must not claim to recover an unobservable author
editing history. Preserve historical strict metrics and ambiguous-occurrence
tests. No canonical-boundary policy or weakened acceptance expectation is
adopted by this plan; a public contract change needs its own explicit
decision before implementation.

## Deferred work and first deliverable

Do not add OCR, a learned semantic model, table recognition, a custom PDF
object parser, a document-wide graph of all possible layouts, new confidence
tiers, or document-specific rules. Do not generalize geometric ordering
without a fixture showing that it is the binding constraint.

The first implementation deliverable is the three-case final-assessment
diagnosis and a minimal reproducible failure. It determines whether the
first core change addresses interval correspondence, one-sided domain
selection, missing anchors, or a different evidenced cause. Do not launch a
new broad recovery rewrite before that result exists.
