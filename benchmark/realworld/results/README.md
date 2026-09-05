# Real-World Revision Benchmark Results

This directory contains immutable, dated, machine-readable summaries for the real-world revision benchmark. Each capture records the engine and corpus state at that date; later captures never overwrite historical metrics.

## Latest Capture

- **Capture date**: 2026-09-05
- **Generator / engine commit**: [`7727975`](https://github.com/hayatosc/pdfdelta/commit/7727975)
- **Environment**:
  - OS: Linux x86_64 (`6.6.87.2-microsoft-standard-WSL2`)
  - Compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
  - Profile: `pdfdelta-bench` release mode
- **Artifact**:
  - File: [`2026-09-05-7727975.json`](2026-09-05-7727975.json)
  - Schema: v64
  - Size: 9,234,821 bytes
  - SHA-256: `c3c65e50df71249b73ab8770967bfbd010292032d4269b7106849a7297c1c962`

The capture contains all 29 manifest pairs. Every pair finished with `ok` status: 19 completed extraction, 10 reproduced their documented incomplete-extraction boundaries, and none stopped at a resource limit or failed. Comparison remains incomplete for every pair.

This capture adds relocated clause-run reporting, its subsumption guard, and
uncertain-line classification reasons, together with six correctness fixes:
silent clause runs no longer meter against the output budget, clause
prefix-candidate selection is deterministic under its cap, `union_consumed`
offsets stay aligned across inserted block separators, roman-numeral list
markers require well-formed syntax within a bounded length, relocated move
spans dedupe blocks with the shared strong check, and a proven `KnownLines`
row order is forwarded to block reconstruction instead of being discarded to a
region-slice fallback.

Reviewed move recall rises from 2/4 to 4/4: the FIPS and EDPB relocated clause
runs now report as moves. Legacy recall follows on those two pairs (0.857 to
1.000 and 0.750 to 1.000). Unmatched tiny changes fall on SP 800-57 (103 to 86)
and FIPS (104 to 102) and rise on no pair. Precision, kind accuracy, and
unresolvable reported spans are unchanged everywhere.

Reading-order coverage is deliberately not the subject of these changes and did
not move: mean comparison coverage across the 19 completely extracted pairs
goes from 59.19% to 59.10%, and unresolved-region count from 31,837 to 31,865.
Reported uncertain changes rise from 525 to 785 because relocated clause runs
surface as `Confidence::Low` cross-page moves rather than as asserted output.

## Reproduction

From a checkout containing the committed artifact, copy the reference aside,
switch to the linked generator commit, generate a summary at a temporary path,
verify provenance, and compare it with the saved reference. Atomic publication
refuses to overwrite existing files:

```bash
cp benchmark/realworld/results/2026-09-05-7727975.json \
  /tmp/pdfdelta-reference-summary.json
git switch --detach 7727975
mise run bench-fetch
mise run bench-revisions-capture -- /tmp/pdfdelta-reproduced-summary.json
mise run bench-revisions-exact-parity -- \
  /tmp/pdfdelta-reproduced-summary.json \
  /tmp/pdfdelta-reference-summary.json
```

The evaluation uses each pair's `limit_scale_hint` from [`manifest.tsv`](../manifest.tsv), without a global `--limit-scale` override.

When a capture only adds sentence-recovery diagnostics, compare it with the
preceding schema through the bounded parity task instead of maintaining an
ad-hoc `jq` filter:

```bash
mise run bench-revisions-schema-parity -- \
  benchmark/realworld/results/2026-09-03-3edac2d.json \
  benchmark/realworld/results/2026-09-03-047786f.json \
  exact_range_parent_outcome
```

The preceding exact-boundary capture can be checked in the same way:

```bash
mise run bench-revisions-schema-parity -- \
  benchmark/realworld/results/2026-08-31-4a77cb4.json \
  benchmark/realworld/results/2026-08-31-87aa0bb.json \
  local_fragment_exact_boundary_trie_shadow
```

The preceding candidate-generation capture can be checked in the same way:

```bash
mise run bench-revisions-schema-parity -- \
  benchmark/realworld/results/2026-08-31-70486f2.json \
  benchmark/realworld/results/2026-08-31-4a77cb4.json \
  local_fragment_length_only_candidate_shadow
```

When reviewed annotations changed between captures, exclude only those named
pairs while preserving exact parity for every other record:

```bash
mise run bench-revisions-schema-parity -- \
  benchmark/realworld/results/2026-08-30-edb34a2.json \
  benchmark/realworld/results/2026-08-31-678e996.json \
  --exclude-pair nist-sp800-57-part1-r4-to-r5 \
  --exclude-pair oasis-mqtt-311-to-50 \
  sentence_edge_signature_direct_shadow
```

## Reviewed-Pair Overview

This capture contains four scoped-complete review sets plus three complete
scopes inside the partial FIPS and SP 800-57 review sets. Precision is available
only inside declared complete scopes; recall and kind accuracy for partial sets
still apply only to their recorded review items.

| Pair | Set | Role | Coverage | Unresolved | Content | Recall | Kind | Hunks / Matched | Tiny |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | dev | standard | 53.65% | 2,291 | 1,057 | 1.000 | 1.000 | 192.286 | 102 |
| `nist-sp800-57-part1-r4-to-r5` | dev | standard | 52.86% | 5,505 | 1,549 | 1.000 | 1.000 | 352.500 | 86 |
| `irs-form-1040-2024-to-2025` | holdout | stress | 45.31% | 79 | 32 | 0.200 | 1.000 | 41.000 | 1 |
| `edpb-right-of-access-v1-to-final` | holdout | standard | 76.21% | 2,142 | 521 | 1.000 | 1.000 | 220.000 | 56 |
| `arxiv-attention-v6-to-v7` | dev | stress | 95.33% | 63 | 1 | 1.000 | 1.000 | 2.000 | 0 |
| `w3c-ws-policy-attach-20060927-to-20061102` | dev | standard | 62.34% | 458 | 148 | 1.000 | 1.000 | 1.000 | 0 |
| `ecma-109-ed10-to-ed11` | dev | stress | 76.34% | 492 | 88 | 1.000 | 1.000 | 53.333 | 5 |
| `oasis-csaf-v2-cs01-to-csd02` | holdout | standard | 73.51% | 841 | 479 | 1.000 | 1.000 | 168.333 | 65 |
| `irs-w4-korean-2024-to-2025` | holdout | stress | 22.70% | 9 | 88 | 0.000 | N/A | N/A | 29 |
| `bis-operational-risk-2011-to-2021` | dev | standard | 77.50% | 722 | 388 | 1.000 | 1.000 | 14.000 | 0 |
| `oasis-mqtt-311-to-50` | dev | standard | 42.48% | 3,495 | 1,615 | 1.000 | N/A | N/A | 0 |
| `nist-csf-v1-1-to-v2-0` | holdout | standard | 68.99% | 1,280 | 794 | 0.333 | 1.000 | 948.000 | 0 |


Schema v60 separates four reviewed outcomes that the legacy recall intentionally
combines. Counts are detected/expected; an em dash means that the review set has
no expectation in that category.

| Pair | Content presence | Exact localization | Semantic relation | Move |
|---|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | 6/6 | — | 6/6 | 1/1 |
| `nist-sp800-57-part1-r4-to-r5` | 8/8 | 3/5 | 8/8 | — |
| `irs-form-1040-2024-to-2025` | 2/5 | — | 1/5 | — |
| `edpb-right-of-access-v1-to-final` | 3/3 | — | 3/3 | 1/1 |
| `arxiv-attention-v6-to-v7` | 1/1 | 1/1 | 1/1 | — |
| `w3c-ws-policy-attach-20060927-to-20061102` | 3/3 | 3/3 | 3/3 | 1/1 |
| `ecma-109-ed10-to-ed11` | 3/3 | 2/2 | 3/3 | — |
| `oasis-csaf-v2-cs01-to-csd02` | 2/2 | — | 2/2 | 1/1 |
| `irs-w4-korean-2024-to-2025` | 1/1 | — | 0/1 | — |
| `bis-operational-risk-2011-to-2021` | 1/1 | 1/1 | 1/1 | — |
| `oasis-mqtt-311-to-50` | — | — | — | — |
| `nist-csf-v1-1-to-v2-0` | 1/3 | — | 1/3 | — |
| **Aggregate** | **31/36** | **10/12** | **29/36** | **4/4** |

The dedicated running-matter path groups exact repeated header or footer
templates, requires at least two same-page old/new occurrences, and accepts a
replacement only when the relation is reciprocal, unique, margin-qualified,
and its minimal edit changes both sides. Exact one-sided containment remains
eligible for deletion or insertion instead of being coerced into a
replacement. ECMA-109 reports the reviewed 2020-to-2025 footer change as one
28-occurrence replacement, holding reviewed recall at 3/3 and exact
localization at 2/2. The EDPB Right of Access consultation watermark remains a
51-occurrence deletion, and its relocated clause run now also reports as a
move, completing its reviewed move expectation.

Across the 19 completely extracted pairs, mean comparison coverage changes
from 59.19% to 59.10%, total reported content events rise from 11,943 to
12,148, and unresolved-region count from 31,837 to 31,865. The region count can
increase when role isolation splits a formerly coarse remainder; it is not a
monotone quality measure. These unreviewed aggregate changes do not by
themselves establish a precision change outside the declared complete scopes.

The current capture proves 215 anchor-bounded changed regions across 11 pairs without
inventing an exact old/new correspondence. These proofs add one reviewed
presence detection for IRS 1040 and one for Korean W-4. The proof feature was
introduced behavior-neutrally in schema v60; later running-matter behavior
changes alter its current aggregate count. Proven regions still do not resolve
source tokens, remove unresolved regions, or create `ChangeEvent`s. NIST CSF
remains at 1/3 presence because none of its unmatched rewrites satisfies the
current conservative anchor-window proof.

The full artifact also records unannotated pairs, extraction boundaries, unresolved token shares, candidate recall, miss diagnostics, and sentence-recovery metrics.
Schema v64 adds a bounded, behavior-neutral diagnostic that joins exact,
candidate-unique ranges of one to eight adjacent recovery-ownership leaves to
strong reciprocal Section parents. All 18 applicable builds complete without a
stop. The nine development builds contain 35 unique exact pairs: none has a
same or changed paired parent, ten have unknown parent evidence, and 25 are
rejected for overlap. FIPS contributes two unique pairs, but one has unknown
parent evidence and one overlaps existing recovery; its only published sample
contains 180 tokens and is not the reviewed 102-token domain-parameter move.
The only two changed-parent samples occur in the OASIS CSAF holdout pair, so
they are not used to tune or activate Move behavior. This capture therefore
provides evidence against productionizing exact parent-change Move under the
current Section proof.

The preceding `2026-09-03-0e071cf.json` capture introduced schema v63 with a
bounded, deterministic review bundle for changed one-to-one
paragraph gaps identified by the schema-v62 section-pairing shadow. Sixteen of
18 applicable builds publish complete bundles; SP 800-57 stops atomically at
the proposal-work limit, and EDPB Right of Access stops atomically because
duplicate structural evidence makes a proposal ambiguous. No partial counters
or samples are published for either stopped build. For that preceding capture,
removing `schema_version` and `section_pairing_proposal_review_bundle` yields
exact JSON parity with `2026-09-03-6390d01.json`. The current capture retains
the same schema but intentionally changes running-matter behavior.

The complete bundles contain 20 proposals: 12 strong and eight number-only.
All 20 have exact bounded edit traces, 12 have leaf-only ownership, seven
overlap existing content events, and one passes the initial structural gate.
The development set contains ten proposals but none passes that gate. Its two
strong proposals are W3C and ECMA paragraph changes that already overlap an
existing event; the ECMA proposal is unrelated to the remaining reviewed
28-occurrence footer-year miss. FIPS and MQTT expose only number-only
proposals. The single passing proposal is in the holdout set and is not used to
tune or activate behavior. Schema v63 therefore provides evidence against
promoting SectionGap recovery under the current structural proof, rather than
claiming an accuracy improvement.

Schema v62 adds a behavior-neutral full-document section-pairing shadow, gated
on the verified recovery-ownership sidecar and reported alongside the
schema-v61 structural-container inventory. The two diagnostics have separate
populations and counters. All 18 applicable section-pairing diagnostics
complete without a stop; the remaining records have no applicable recovery
ownership sidecar.
Removing `schema_version` and `section_pairing_shadow` yields exact JSON parity
with `2026-09-02-5ed948e.json`.

The completed shadows pair 1,702 exact headings and 30 headings matched after
stripping their numbering. They classify 342 pairs as strong and 1,390 as
number-only. Strong-pair topology is conservative: 157 pairs are monotone,
none is crossing, and 185 remain topology-unknown. Strong paragraph gaps
contain 125 changed one-to-one candidates, but only 15 place both paragraphs in
the same unresolved span. The number-only view contains 106 changed one-to-one
candidates, 42 of them in the same unresolved span.

The strong same-unresolved evidence does not yet justify a behavior change.
Among development pairs, only W3C WS-Policy and ECMA-109 expose one such
candidate each. The fully reviewed W3C scope already detects every expected
change. ECMA's remaining reviewed miss is the 28-occurrence repeated-footer
year replacement, while schema v62 records aggregate counts rather than source
ranges or occurrence identity, so it cannot prove that the candidate is that
footer change. FIPS 186 and SP 800-57 have same-unresolved candidates only in
the number-only view, and NIST CSF has no section pairs. Candidate-level ranges
and deterministic review evidence are required before any section-pairing
proposal is promoted to comparison behavior.

Schema v61 adds a behavior-neutral structural-container inventory over verified
recovery ownership. All 18 applicable recovery builds complete without a stop;
the extraction-complete GCC pair has no sentence-recovery ownership partition,
so the diagnostic is not applicable there. The complete builds identify 706
old and 771 new conservative numbered sections plus 22,685 old and 25,990 new
paragraph blocks. They reject 6,854 old and 7,114 new blocks whose ownership,
role, mapping, run, page, or font evidence is unsafe. Section counts differ
substantially in several revisions, so this capture does not enable structural
alignment behavior. It establishes the evidence baseline for a separate
section-pairing shadow. Removing `schema_version` and
`structural_container_shadow` yields exact JSON parity with schema v60.

Schema v60 adds anchor-bounded `ProvenChangedRegion` output and separate
content-presence, exact-localization, semantic-relation, and move recall. The
proof compares exact canonical token multiplicities only inside fully owned,
source-backed, extraction-complete main-anchor windows. It is order-independent
and fails closed on normalization, unstable unmapped evidence, projection,
ownership, identity, or resource uncertainty. A proven region contributes to
the CLI content-difference decision but remains unresolved and does not affect
coverage. Removing `schema_version`, `reviewed_recall_metrics`, and
`reported_proven_changed_regions` yields exact JSON parity with the corrected
schema-v59 capture.

Schema v59 adds a behavior-neutral paired-stream sequence-relation shadow.
Removing that field and the schema number produces exact JSON parity with
schema v58. All 16 applicable shadows complete without a resource stop. They
contain 794 qualified Sentence edges, 43 edges rejected by the local relation,
781 canonical-path edges, and 775 globally forced edges. After preserving
sub-threshold competitor evidence and the existing crossing and exact-tail
gates, none of the locally rejected edges is adoptable. Twenty-five forced
edges have a disqualifying external competitor and five hit an existing
downstream veto. Candidate-weighted veto evidence is dominated by
disqualifying competitors: 41,072 source tokens, compared with 4,889 for
reciprocal failure, 2,107 for tied best scores, 1,971 for crossing relations,
and 1,029 for insufficient margins. The sequence rule therefore remains
diagnostic and must not be enabled as a recovery behavior.

Schema v58 adds a behavior-neutral shadow that projects exact adjacent segments
and alignment anchors onto source-backed trusted-line ranges. Removing the new
topology fields and schema number produces exact JSON parity with schema v57.
Across the four reviewed pairs with unique exact segments, 768 pairs are
classified: 77 monotone, one crossing, and 690 topology-unknown. The remaining
FIPS domain-parameter segment is still unknown because its compatible stream
pair has ambiguous partners; it has no crossing-anchor evidence and is not
promoted to `Move`.

The preceding schema-v57 capture tightened reviewed cross-kind matching without
changing comparison behavior. An actual event whose kind differs from the
expected annotation may now claim it only when source-backed `AtomicEdit`
evidence of that expected kind contains the expected quote. Replacement
evidence must come from the same semantic hunk; relation context and
word-completed display spans are not accepted as edit evidence. All 28
non-ECMA records are field-identical to `61981dc`. ECMA preserves every
comparison field, but its copyright-notice insertion is now honestly reported
as `alignment_or_candidate`: recall changes from 0.667 to 0.333, while kind
accuracy changes from 0.500 to 1.000 because the unrelated replacement no
longer claims the missed insertion.

Schema v57 preserves every comparison, coverage, change, quality, complete-
scope, and candidate-recall field from schema v56. It adds a bounded relation
trace only when a reviewed expected change claims an actual event of the wrong
kind. The trace records origin, alignment spans, token counts, exact-edit hunk
shape, and score evidence without serializing PDF text. ECMA-109's expected
copyright-notice insertion claims a `sentence_near` replacement whose old/new
relation contexts contain 355/354 tokens. The emitted semantic change contains
two old tokens and one new token, split into one deletion-only hunk and one
replacement hunk; the expected notice occurs in the relation context but in no
semantic or insertion-only hunk. This proves the current wrong-kind result is
an over-broad evaluation claim, not evidence that the insertion was detected.
It does not yet prove a safe containment relation for production alignment.

Schema v56 uses the v55 recovery-leaf inventory to recover globally unique,
token-verified `TrustedRunResidual` ranges as unchanged exact matches. All 18
available recovery builds complete this stage without a typed stop. The stage
selects 2,767 matches and resolves 192,030 additional source tokens on each
side without emitting a content event. Across the 19 completely extracted
pairs, mean comparison coverage rises from 55.42% to 59.00% and the median
rises from 56.81% to 62.32%. Of those 19 pairs, 14 improve, five remain
unchanged, and none regress; the 10 extraction-incomplete pairs retain
unavailable comparison coverage. The largest gains are QGIS English (+22.91
points), QGIS Spanish (+17.57), LibreOffice (+8.81), OASIS CSAF (+7.03), and
W3C WS-Policy (+5.51).

All 29 pairs preserve their content, formatting, and uncertain event counts,
reviewed quality metrics, complete-scope metrics, and candidate recall. The
exact matches split some remaining unresolved spans, so the unresolved-region
count rises by 2,819 even though resolved token coverage increases. Comparison
therefore remains incomplete for all pairs; the higher region count is a
representation effect, not evidence that more source tokens became unresolved.

Schema v55 adds behavior-neutral, source-backed recovery ownership, committed
change-origin attribution, and a deterministic local-fragment review bundle.
All 18 available recovery builds partition their eligible evidence without a
stop: 5,737,957 tokens split into 2,839,962 accepted tokens, 2,741,232
unselected leaf tokens, and 156,763 typed-gap tokens. Trusted-run residuals are
the largest actionable leaf class at 1,253,366 tokens across 47,244 ranges.
The bundle validates all 66 finally committed local-fragment replacements and
their source evidence; 68 proposals were considered, while two SP 800-57
proposals were correctly removed by exact diff or span projection before the
atomic output commit. Removing the v55-only fields and the corrected final
commit counter produces exact behavior parity with schema v54.

```bash
mise run bench-revisions-schema-parity -- \
  benchmark/realworld/results/2026-09-02-46d4b9e.json \
  benchmark/realworld/results/2026-09-02-3ec54e5.json \
  --ignore-field local_fragment_proposals_committed \
  change_origins \
  recovery_leaf_partition_complete \
  recovery_leaf_partition_stop_reason \
  recovery_ownership_partition \
  local_fragment_review_bundle
```

Relative to `d6ad8c3`, a secondary exact-only pass now recovers globally unique
completed sentences from clean source-backed ranges inside blocks whose other
ranges carry normalization or unmapped evidence. The primary relation graph is
frozen before this pass, so these matches add resolved context without changing
content events or relation decisions. Exact-match coverage increases in five
pairs:

| Pair | Coverage change | Exact tokens added per side |
|---|---:|---:|
| `arxiv-attention-v6-to-v7` | 89.50% -> 92.11% | 1,012 |
| `w3c-ws-policy-attach-20060927-to-20061102` | 56.68% -> 56.81% | 128 |
| `oasis-csaf-v2-cs01-to-csd02` | 65.25% -> 66.57% | 3,704 |
| `qgis-pyqgis-en-328-to-334` | 58.80% -> 60.78% | 5,925 |
| `qgis-doc-guidelines-es-328-to-334` | 63.20% -> 64.99% | 1,600 |

Across the 19 completely extracted pairs, mean coverage rises from 55.00% to
55.42% and the median rises from 56.68% to 56.81%. The pass resolves 12,369
additional exact tokens on each side. All 29 pairs preserve their content,
formatting, and uncertain event counts, quality metrics, complete-scope metrics,
and candidate recall. The five changed pairs add 205 unresolved regions because
recovering interior exact ranges partitions the remaining unresolved spans;
their stale remainder attribution is therefore marked `analysis_incomplete`.
No resource-limit, near-relation, or fallback outcome changes.

Relative to `8d80c64`, production fragment recovery no longer stops after
three otherwise accepted proposals in one recovery build. It selects all 68
strict proposals and finally commits 66 instead of 19. Four pairs change: FIPS gains 0.21
coverage points, SP 800-57 gains 0.68, EDPB Dark Patterns gains 1.21, and
LibreOffice gains 0.03. Mean coverage across the 19 comparable pairs rises
from 54.89% to 55.00%, while the 56.68% median remains unchanged. The extra
49 proposals resolve 12,325 source tokens, only 0.42% of the preceding
2,935,058-token recovery remainder. Reported content events rise by 47 and
unresolved regions by 78 because the recovered fragments partition remaining
regions. Reviewed recall, kind accuracy, and every complete-scope event/token
metric remain unchanged; the newly reported events outside complete scopes do
not establish corpus-wide precision.

All 18 available recovery builds complete the unresolved-token partition. Of
2,922,733 unresolved source tokens, 1,434,867 (49.09%) are not owned by a
located recovery unit and 1,134,433 (38.81%) remain behind a near-relation
veto. Together these two classes account for 87.91% of the recovery remainder.
The dominant unlocated mass is concentrated in LibreOffice (471,969 tokens),
QGIS English (182,774), SP 800-57 (126,499), EDPB Dark Patterns (122,643), and
OASIS CSAF (109,618). The next coverage work must therefore distinguish safe
trusted-stream fragments from genuinely unlocated evidence before changing
near-relation thresholds.

| Pair with a complete scope | Event P / R / F1 | Token P / R / F1 | Span IoU | FP tokens / 10k unchanged |
|---|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | 1.000 / 0.000 / 0.000 | 0.000 / 0.000 / 0.000 | 0.000 | N/A |
| `nist-sp800-57-part1-r4-to-r5` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | 0.000 |
| `arxiv-attention-v6-to-v7` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | 0.000 |
| `w3c-ws-policy-attach-20060927-to-20061102` | 1.000 / 1.000 / 1.000 | 0.984 / 1.000 / 0.992 | 0.984 | 68.027 |
| `bis-operational-risk-2011-to-2021` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | 0.000 |
| `oasis-mqtt-311-to-50` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | 0.000 |

In the preceding `8d80c64` capture, relative to schema-v53 `82c882c`, isolated
exact-tail recovery commits 91 globally unique matches and resolves 5,750
tokens on each side.
Mean comparison coverage across the 19 comparable pairs rises from 54.78% to
54.89%, while the median remains 56.68%. Seven pairs improve; the largest gains
are QGIS English (+1.29 points), QGIS Spanish (+0.49), arXiv Attention (+0.15),
and LibreOffice (+0.12). Content-event counts and every reviewed event/token
quality metric remain unchanged. Unresolved region count rises by 83 because
removing exact tails splits some remaining regions.

The global multiplicity census examines 61,271 units and 7,235,556 evidence
tokens before verifying 5,833 candidate tokens. This is deliberately isolated
from ordinary exact and near relations, but its cost is large relative to the
0.11-point mean coverage gain. Future work should reuse an existing exact index
or narrow the census without weakening the global uniqueness proof.

The preceding trusted-tail promotion from `0363ab5` to `82c882c` raised mean
comparison coverage from 51.73% to 54.78% and the median from 53.22% to 56.68%.
That larger step also improved reviewed recall for ECMA-109 and IRS 1040. The
schema-v54 tail enrichment is intentionally narrower and does not alter those
relation decisions.

These coverage gains do not establish corpus-wide precision. Across the same
19 extraction-complete pairs, unresolved regions rise from 25,193 to 27,743
because recovered runs partition the remaining evidence, and reported content
events rise from 11,048 to 12,122. Most of those events are outside complete
scopes. The next evaluation must classify the remaining unresolved token mass
and review representative gained regions before broadening recovery further.

The engine now treats a clean trailing fragment from one trusted line run as a
Sentence recovery unit when the run contains no sentence terminator. The same
boundary remains independent fragment-veto evidence, and both owned token
copies are budgeted. Untrusted runs, normalization issues, unmapped tokens,
ordinal gaps, and unsupported line barriers remain fail-closed. The behavior
therefore uses only order within one trusted run and does not infer order
between columns or regions.

The engine recovers the remaining SP 800-57 edit across a trusted pair of
adjacent Sentence units and one Line unit. It requires compatible roles,
bounded punctuation or ASCII-case substitutions, exact context on both sides,
and a globally unique exact-or-near counterpart. The complete recovery plan is
discarded on resource, allocation, projection, or overlap failure. The two
atomic edits are reported as exact spans while their surrounding context is
counted only as resolved coverage.

The preceding schema-v52 capture promoted only reciprocal, role-compatible local fragments with one
exact interior token edit. Existing recovery ranges and the shared output
budget are checked before the three-proposal cap is applied; allocation,
projection, or analysis failure commits no partial fragment plan. All 18
available proposal analyses complete in this capture. They consider 66 ranked
proposals and commit 18 across eight pairs.

Relative to schema v50, mean comparison coverage across the 19 comparable
pairs moves from 51.6715% to 51.7270%, total content events move from 11,789 to
11,807, and comparison completion remains 0/29. The reviewed change is SP
800-57's `toolkit-footnote-comma-removed`: its global reviewed recall rises
from 0.750 to 0.875 while kind accuracy remains 1.000. The other reviewed-pair
recall and kind values do not change. SP 800-57's hunks-per-matched value falls
because one more expected item is matched; the capture does not show a global
hunk-count reduction. Unresolved-region counts rise where one recovered
interior fragment partitions the remaining unresolved range, even though
resolved token coverage increases. Most of the 18 new events are outside
complete scopes, so this result establishes the reviewed recall gain but not
corpus-wide precision.

The latest engine resolves a recovered near relation over its full unit while
reporting only bounded-Myers atomic edit hunks as changed spans. Diff,
projection, or output-budget failure atomically returns the relation to
unresolved instead of emitting a whole-unit fallback. Instrumented comparisons
retain the exact recovered edit trace used by the public change event.

Complete-scope annotations distinguish stable relation context from one or
more exact changed ranges. This corrected an incomplete SP 800-57 review item:
the existing `Approved` relation contains a 31-token lead-in rewrite in addition
to its comma deletion. With the complete atomic ranges recorded, scoped token
precision rose from 0.092 to 1.000, recall from 0.664 to 0.895, F1 from 0.162
to 0.944, and false-positive density fell from 6,372.166 to zero. The current
capture recovers the four formerly unresolved Association punctuation tokens.

The preceding schema-v50 capture is behavior-neutral relative to schema v49.
Removing the diagnostic-only
`sentence_recovery_metrics.local_fragment_flat_exact_boundary_shadow` field
from schema v50 and removing `schema_version` produces exact parity for every
prior comparison, quality, candidate, recovery-diagnostic, and scoped metric
field.

The new shadow replaces the shared exact-token transition trie with
side-separated, depth-local exact classes. Old and new fragments share each
class namespace, so equal class IDs still certify exact prefix or suffix token
sequences. Checked `u32` offsets and class IDs retain the persistent result;
bounded stable radix passes build each depth without a transition `HashMap`.
The shadow accounts for both reusable radix buffers and the active-fragment
index, verifies every hash candidate against exact classes, and reuses the
unchanged threshold-capped edge recheck. Any resource or allocation stop
discards partial classes and publishes only immutable configuration plus
stop-safe work.

The flat and trie shadows are both available for all 18 recovery builds and
complete on the same 11. Five builds stop at the exact-recheck comparison
limit, while MQTT and LibreOffice stop at the candidate-union limit. Across
the 11 both-complete builds, all 12 count, order, certification, and retained
fingerprint mismatch totals are zero. The 6,540,013 hash candidates are all
exact-certified, with no observed hash-only collision.

| Complete-build evidence | Value |
|---|---:|
| Exact recheck comparisons | 31,793,303 |
| Certified comparisons avoided | 41,372,963 |
| Retained pairs | 1,946,919 |
| Exact class slots | 8,039,524 |
| Persistent class storage | 32,859,704 bytes |
| Radix scratch storage | 9,822,008 bytes |
| Prefix / suffix distinct classes | 1,167,925 / 1,235,627 |
| Active-fragment visits | 4,195,140 |
| Radix work | 241,185,720 |
| Prefix / suffix / both certifications | 3,727,995 / 2,757,614 / 54,404 |
| Depth 1 / 2-3 / 4+ candidates | 0 / 2,073,747 / 4,466,266 |

The v49 exact representation used 96,023,032 logical trie bytes plus
64,316,192 bytes of per-fragment boundary-node arrays. The v50 class storage
and scratch use 42,681,712 bytes, a reduction of 117,657,512 bytes (73.38%).
Total bounded estimated bytes across the same builds fall from 361,283,280 to
243,625,768, a 32.57% reduction. On the ten builds that also complete in
schema v48, the added storage overhead above the v48 path falls from 76.61% to
19.45%; this removes 74.61% of the representation overhead introduced by the
trie shadow.

This compact representation does not complete any additional build because
the remaining stops occur after class construction. It also performs exactly
30 radix operations per class slot, and the capture does not measure wall-clock
time or peak RSS. The result therefore validates the memory model and retained
stream, not production readiness. Exact-recheck and candidate-union work remain
the performance bottlenecks; scoped token metrics separately show that
recovered replacements must emit exact changed spans before more fragment
relations are promoted.

Schema v49 is behavior-neutral relative to schema v48. Removing the
candidate-only
`sentence_recovery_metrics.local_fragment_exact_boundary_trie_shadow` field
from schema v49 and removing `schema_version` produces exact parity for every
prior comparison, quality, candidate, recovery-diagnostic, and scoped metric
field.

The new shadow preserves schema v48 candidate generation and retained-pair
ordering. It adds one shared exact-token boundary trie for old and new local
fragments. Rolling hashes still generate candidates only; matching trie node
identities certify exact prefix or suffix ranges, and the exact recheck skips
only comparisons already proved by those identities. Stopped runs publish only
immutable configuration and stop-safe work. Empty fragment sets complete
without allocating a root node and retain the same empty pair-stream
fingerprint as schema v48.

The shadow is available for all 18 recovery builds and completes 11. Five of
the remaining builds stop at the exact-recheck comparison limit; MQTT and
LibreOffice reach the later candidate-union limit after certified comparisons
let traversal progress farther. The empty arXiv and Korean W-4 builds complete
with zero fragments, trie nodes, work, and candidates. Across all 11 complete
builds, 6,540,013 hash candidates are all exact-certified, with no observed
hash-collision-only candidate.

| Complete-build evidence | Value |
|---|---:|
| Exact recheck comparisons | 31,793,303 |
| Certified comparisons avoided | 41,372,963 |
| Retained pairs | 1,946,919 |
| Trie nodes / transitions | 2,400,583 / 2,400,574 |
| Trie token steps | 8,039,524 |
| Trie logical bytes | 96,023,032 |
| Total bounded logical work bytes | 361,283,280 |
| Prefix / suffix / both certifications | 3,727,995 / 2,757,614 / 54,404 |
| Depth 1 / 2-3 / 4+ candidates | 0 / 2,073,747 / 4,466,266 |

Ten builds complete under both schema v48 and schema v49. Two are zero-work
builds; across the remaining evidence, retained count, order, and fingerprint
parity are exact.

| Both-complete evidence | Schema v48 | Schema v49 | Change |
|---|---:|---:|---:|
| Candidate pairs | 3,430,113 | 3,430,113 | 0 |
| Retained pairs | 1,134,831 | 1,134,831 | 0 |
| Exact token comparisons | 40,898,423 | 17,593,211 | -23,305,212 (-56.98%) |
| Bounded logical work bytes | 154,479,672 | 272,822,104 | +118,342,432 (+76.61%) |
| Trie logical bytes | 0 | 72,133,584 | +72,133,584 |

For every both-complete build, schema-v49 comparisons plus reported avoided
comparisons equal the schema-v48 count exactly. FIPS is newly complete in
schema v49: it performs 14,200,092 comparisons and certifies 18,067,751 more,
for the same projected 32,267,843 comparisons that exceeded the schema-v48
budget. Its 3,109,900 candidates retain 812,088 pairs without a hash-only
collision.

The comparison savings are substantial, but the 76.61% increase in bounded
logical work bytes means this representation is not ready for production.
These are accounting-model bytes rather than process RSS. The next diagnostic
should reduce exact-trie storage or replace it with a more compact exact
boundary representation while preserving the same retained stream and
comparison-savings equation.

Schema v48 is behavior-neutral relative to schema v47. Removing the
candidate-only
`sentence_recovery_metrics.local_fragment_length_only_candidate_shadow` field
from schema v48 and removing `schema_version` produces exact parity for every
prior comparison, quality, candidate, recovery-diagnostic, and scoped metric
field.

The new shadow retains the fixed-depth parent-admission constraint but does not
build or query the global fixed-depth fragment candidate stream. It enumerates
only the length-aware own/all-depth postings, preserves their sorted and
deduplicated candidate order, and applies the same threshold-capped exact edge
recheck under an independent resource budget. Stopped runs publish only
immutable configuration and stop-safe work.

The shadow is available for all 18 recovery builds and completes ten, up from
seven in schema v47. EDPB Dark Patterns, BIS Operational Risk, and NIST CSF are
the three newly complete builds. All eight remaining runs now stop at the exact
recheck comparison limit; the ten candidate-union stops observed in schema v47
are eliminated.

Across the seven builds where both shadows complete, parity is available and
all four count plus two fingerprint mismatch totals are zero. Both paths
produce the same 326,225 pre-recheck candidates, 5,810,109 successful token
comparisons, and 170,148 retained pairs. Omitting the fixed global stream
reduces candidate-union work from 1,295,809 to 326,225 items (74.82%) and
posting visits from 1,388,457 to 404,274 (70.88%). Built posting items fall from
963,163 to 929,671 (3.48%); query and exact-recheck work remain identical.

Across all ten complete length-only shadows, 3,430,113 candidate pairs consume
40,898,423 successful token comparisons and retain 1,134,831 pairs. The
candidate-generation optimization is therefore validated on both dev and
holdout records without changing comparison behavior. The next diagnostic
should target the exact-recheck rejection and comparison cost that now limits
the remaining eight builds, while using only dev records for tuning decisions.

Schema v47 is behavior-neutral relative to schema v46. Removing the
candidate-only
`sentence_recovery_metrics.local_fragment_length_aware_recheck_shadow` field
from schema v47 and removing `schema_version` produces exact parity for every
prior comparison, quality, candidate, recovery-diagnostic, and scoped metric
field.

The new shadow rechecks only candidates present in the length-aware fragment
stream while preserving the same exact token-edge gate and an independent
resource budget. It still constructs both fixed-depth and length-aware
candidate streams so their fingerprints and retained order can be checked
against the existing reuse shadow. A stopped run publishes only immutable
configuration and stop-safe work; no partial relation is exposed or used by
the comparison.

The shadow is available for all 18 recovery builds and completes seven, up
from five for the reuse shadow. W3C WS-Policy and OASIS CSAF are the two newly
complete builds. Ten of the remaining runs stop at the candidate-union limit;
ECMA-109 is the sole run that reaches the exact-recheck comparison limit.
Across the five builds where both shadows complete, all three candidate and
retained fingerprint mismatch counters are zero. Rechecks fall from 12,219 to
8,845 (27.61%), and successful token comparisons fall from 299,706 to 273,175
(8.85%), exactly removing the 3,374 fixed-only rejects and their 26,531
comparisons observed in schema v46.

Across all seven complete length-aware recheck shadows, 326,225 pairs consume
5,810,109 successful token comparisons and retain 170,148 pairs. Their
candidate construction still examines 1,295,809 union items because this
diagnostic intentionally builds both streams. The corpus therefore supports
removing fixed-only exact rechecks, but the ten candidate-union stops show that
the next behavior-neutral step must avoid constructing the fixed-depth stream
itself before any production behavior changes.

Schema v46 is behavior-neutral relative to schema v45. Removing
`sentence_recovery_metrics.local_fragment_recheck_reuse_shadow` from both
captures and removing `schema_version` produces exact parity for every prior
comparison, quality, candidate, recovery-diagnostic, and scoped metric field.

The recheck-reuse shadow now attributes every threshold-capped exact edge
recheck to a candidate-source membership: shared by the fixed-depth and
length-aware indexes, fixed-depth only, or length-aware only. Started and
completed pairs, accepted and rejected outcomes, and successful token
comparisons remain available after a resource stop. Per-membership totals must
sum to the existing aggregate counters, at most one pair may remain in flight,
and each completed pair must own at least one successful comparison.

Across all 18 available builds, 9,559,129 shared pairs complete: 2,724,326 pass
the edge gate and 6,834,803 fail it. Another 37,124,333 fixed-only pairs
complete, and every one fails the exact gate. No length-aware-only candidate is
observed. The fixed-only pairs account for 79.52% of started pairs and
244,870,953 of 342,402,874 successful comparisons (71.52%). The same zero
non-shared-accept result holds independently in the dev and holdout splits.

The five complete builds contain 8,845 shared and 3,374 fixed-only pairs. All
7,806 accepted pairs are shared; the fixed-only rejects consume 26,531 of
299,706 comparisons (8.85%). The 13 stopped builds preserve only stop-safe work
attribution and show the same outcome over their completed prefix: all
37,120,959 completed fixed-only pairs reject. This evidence supports a separate
behavior-neutral candidate-source pruning shadow; it does not itself change
fragment recovery or authorize publishing partial stopped results.

Schema v45 is behavior-neutral relative to schema v44. Removing
`sentence_recovery_metrics.local_fragment_recheck_reuse_shadow` from both
captures and removing `schema_version` produces exact parity for every prior
comparison, quality, candidate, recovery-diagnostic, and scoped metric field.
Use `--ignore-field local_fragment_recheck_reuse_shadow` with the schema-parity
mise task for this comparison.

The recheck-reuse shadow now stops an exact token-edge scan as soon as the
existing integer edge score reaches 3,000 basis points. The score is monotone as
matching prefix or suffix evidence grows, so later comparisons cannot reverse
an accepted decision. Rejected pairs still scan exactly as before. Exhaustive
small binary-token tests and every available sibling fingerprint preserve the
fixed-depth and length-aware candidate and retained streams.

The same five of 18 recovery builds complete: two have no eligible fragments,
and IRS 1040 plus the QGIS English and Spanish pairs provide non-empty results.
Across those three results, the same 12,219 unique pairs now consume 299,706
exact token comparisons instead of schema v44's 849,004, a 64.70% reduction.
Against the original duplicated two-stream projection of 1,671,477 comparisons,
threshold capping plus merge-union reuse removes 82.07%. Within the capped path
alone, reuse reduces the duplicated projection from 572,881 comparisons to
299,706, avoiding 273,175 comparisons (47.68%).

IRS 1040 falls from 68,505 comparisons to 42,193 (38.41%), QGIS English from
689,972 to 222,947 (67.69%), and QGIS Spanish from 90,527 to 34,566 (61.82%).
Their retained counts and ordering remain unchanged.

The other 13 builds still stop atomically at the exact-recheck comparison limit.
At the same aggregate limit of 342,103,168 comparisons, they progress from
43,106,010 to 46,671,256 unique pairs (8.27%) and from 51,870,391 to 56,231,629
candidate items (8.41%). Stop-safe attribution shows that 2,716,520 completed
pairs pass the edge gate and 43,954,723 fail it: 94.18% of completed pairs still
require the rejecting scan. Candidate-source attribution records 9,559,975
shared fixed/length-aware candidates, 37,132,743 fixed-only candidates, and no
observed length-aware-only candidate across all 18 builds. This corpus result
does not prove the subset relation under signature collisions. The next
behavior-neutral diagnostic should classify accepted and rejected pairs by
candidate-source membership before any candidate stream is pruned.

Schema v44 is behavior-neutral relative to schema v43. Removing only
`sentence_recovery_metrics.local_fragment_recheck_reuse_shadow` and
`schema_version` produces exact parity for every prior comparison, quality,
candidate, recovery-diagnostic, and scoped metric field.

The recheck-reuse shadow merges the sorted fixed-depth and length-aware
candidate sets for each old fragment. Every unique old/new pair receives one
exact token-edge recheck, and the result is routed back to each source stream
without assuming that either candidate set is a subset of the other. This
preserves collision safety, candidate order, retained order, and atomic stop
behavior while measuring the duplicated work avoided by reuse.

The shadow is present on all 18 recovery builds. Five complete: arXiv and
Korean W-4 have no eligible fragments, IRS 1040 remains the directly comparable
non-empty baseline, and the QGIS English and Spanish documentation pairs now
complete under the unchanged limits. The other 13 stop atomically at the exact
edge-recheck comparison limit, down from 15 in the schema-v43 global shadow.

Across the three non-empty complete builds, the legacy two-pass projection is
21,064 rechecks and 1,671,477 token comparisons. Merge-union reuse performs
12,219 rechecks and 849,004 comparisons, avoiding 8,845 rechecks (41.99%) and
822,473 comparisons (49.21%). No complete build has a length-aware-only
candidate, retained-set mismatch, order mismatch, or available sibling
fingerprint mismatch.

IRS 1040 provides the exact schema-v43 comparison: 3,714 unique pairs are
rechecked once, 2,293 duplicate rechecks are avoided, and token comparisons
fall from 123,765 to 68,505, a 44.65% reduction. Both streams still retain the
same 1,725 pairs in the same order, and all four canonical fingerprints match.
The QGIS English and Spanish builds avoid 49.57% and 49.58% of projected token
comparisons respectively; their schema-v43 siblings stopped before publishing
fingerprints, so they are new completion evidence rather than direct sibling
parity claims.

This is still behavior-neutral diagnostic evidence. It justifies folding the
merge-union recheck primitive into the global fragment analysis, but not using
fragment relations in recovery output. The remaining 13 stops show that exact
edge comparison is still the limiting stage for larger builds.

Schema v43 is behavior-neutral relative to schema v42. Removing only
`sentence_recovery_metrics.local_fragment_global_length_aware_shadow` and
`schema_version` produces exact parity for every prior comparison, quality,
candidate, recovery-diagnostic, and scoped metric field.

The global length-aware local-fragment shadow removes the new-parent identity
from boundary-signature keys. It performs one boundary query per old fragment,
then filters each posting by the parent candidates admitted by the unchanged
parent index. Four canonical SHA-256 streams compare fixed-depth and
length-aware candidates and retained pairs with the parent-scoped schema-v42
shadow. All available cross-sibling fingerprints match, including pair order.

The shadow is present on all 18 recovery builds. The same three builds complete:
arXiv and Korean W-4 have no eligible fragment, and IRS 1040 is the only
non-empty complete build. The other 15 now stop atomically at the exact
edge-recheck comparison limit. The seven former query-limit stops are gone.

Across all bounded work, query count falls from 20,489,679 in the parent-scoped
shadow to 42,147 in the global shadow, a 99.79% reduction. The stopped builds
reach much farther into candidate rechecking, so their other cumulative work
counters are not like-for-like totals: posting visits rise from 29,508,426 to
43,839,878, candidate union from 26,535,350 to 39,453,324, and exact
comparisons from 235,378,311 to 343,335,669. This is evidence that query
repetition is removed and the next limiting stage is exposed, not evidence that
the complete algorithm performs more work.

On the complete IRS 1040 build, parent-scoped and global work are directly
comparable. Total queries fall from 2,010 to 215, an 89.30% reduction; 208 are
global boundary queries and seven are parent-admission queries. Both paths
visit 6,497 postings, submit 6,007 candidates, perform 123,765 exact
comparisons, and retain the same 1,725 pairs in the same order. The global
capacity estimate falls from 1,548,320 to 1,396,000 bytes, and logical bytes
fall from 948,856 to 820,536.

This remains behavior-neutral evidence and is not sufficient to connect global
traversal to recovery behavior. Production use must wait until complete prose
results exist. The next shadow should avoid repeating exact edge comparisons
for length-aware candidates already rechecked by the fixed-depth parity path,
without changing thresholds, candidate order, or atomic stop behavior.

Schema v42 is behavior-neutral relative to schema v41. Removing only
`sentence_recovery_metrics.local_fragment_length_aware_shadow` and
`schema_version` produces exact parity for every prior comparison, quality, candidate,
recovery-diagnostic, and scoped metric field.

The length-aware local-fragment shadow is available on all 18 recovery builds.
Three complete: arXiv and Korean W-4 have no eligible one-sided Sentence
fragment, while IRS 1040 provides the only non-empty complete result. The other
15 stop atomically before publishing parity sets: seven at the query limit and
eight at the exact edge-recheck comparison limit. No partial candidate or
parity counters survive those stops.

On IRS 1040, own/all-depth signatures reduce 3,714 fixed-depth candidates to
2,293 before exact edge recheck, a 38.26% reduction. Both paths retain the same
1,725 pairs with zero missing pairs, extra pairs, or order mismatches. That
complete build contains 680 fragments, reports 948,856 logical bytes and a
1,548,320-byte capacity estimate, and has a largest posting of 61 items. This is
not sufficient evidence to connect the index to recovery behavior: the only
non-empty complete build is a holdout stress form, and 15 prose-heavy builds do
not reach a complete parity result. The next step must reduce or better
attribute shadow query and recheck work without changing thresholds, margins,
or comparison output.

The annotation-independent local-fragment shadow is available on 18 recovery
builds. Two complete with no eligible one-sided Sentence parent. Every non-empty
build still stops atomically, now uniformly at the similarity-comparison limit.
The boundary index stores 838,444 same-side fragment signatures and performs
3,799,185 admitted-parent fragment queries with 2,863,380 posting visits. It
submits 2,840,908 candidates to exact edge recheck. Across the bounded work
retained before each schema's stops, that is 71.23% fewer candidate pairs than
schema v39's 9,874,942-pair Cartesian expansion, even though v40 reaches 87,646
admitted parent pairs instead of 12,842 before stopping.

The index therefore removes the fragment-pair Cartesian bottleneck, but exact
edge rechecks and word scoring now consume the comparison budget. Schema v41
attributes the 42,939,833 examined comparisons without changing that shared
limit: prefix and suffix edge scans consume 22,321,481 (51.98%), word-range
scans consume 19,867,796 (46.27%), and word merges consume 750,556 (1.75%).

Of the 2,840,902 fragment pairs whose edge scan completed, 137,413 (4.84%) pass
the 3,000-point gate. Only 519,299 (18.28%) satisfy the weaker mathematical
condition required by a length-aware prefix or suffix signature. An exact
length-aware candidate primitive could therefore reject 81.72% of the current
fixed-depth pairs before their full edge recheck, subject to separate index
memory, collision, ordering, and resource-stop validation. The next change
should remain a behavior-neutral shadow; thresholds and recovery behavior stay
unchanged. Stopped reports retain configuration and bounded work only and
discard every partial relation set and sample.

The preceding schema-v37 quote-local diagnostics complete without a stop in
all eight recovery-watch reports. Their ten watched Sentence pairs are all
available on both sides and consume 20,310 comparisons, 309,018 bounded
edit-work units, 149 output items, and 411 output scalars.

The unresolved SP 800-57 toolkit-footnote replacement is the only missed item
that combines a recovery location on both sides, a 10,000-point local score,
and a small exact content edit: one comma is deleted. Its enclosing Sentence
pair still scores 2,347 and is non-reciprocal because both parents have tied
10,000-point competitors. Other missed local evidence is either exact unchanged
text, lacks a recovery location, or has a very low score and large edit.
Schema v40 shows that fixed-depth boundary enumeration removes most candidate
pairs but does not finish within the comparison budget. Schema v41 supplies the
comparison-stage attribution; the next evidence-neutral step is a length-aware
signature shadow before reciprocal fragment evidence can influence recovery
behavior.

The original arXiv, W3C, and BIS scoped-complete metrics are unchanged from
`27f096e`.
The new MQTT permissions scope contains no changes and reports no false-positive
event or token. Direct signature traversal is accepted and complete for all 18
available recovery builds. It performs zero broad Sentence posting visits and
eliminates all six former full-build legacy fallbacks. On those six paths, the
completed schema-v34 reference oracles classified 31,842,858 broad candidates;
production Direct generates 1,633,897 candidates, a 94.87% reduction. The 12
paths that were already complete preserve their comparison and quality fields.

Completing the other six paths changes coverage and content-event counts in
both directions:

| Pair | Coverage | Content events |
|---|---:|---:|
| `nist-fips-186-4-to-5` | 65.03% -> 51.20% | 1,346 -> 1,003 |
| `nist-sp800-57-part1-r4-to-r5` | 71.90% -> 52.72% | 2,440 -> 1,566 |
| `nist-sp800-171-r2-to-r3` | 4.45% -> 47.97% | 46 -> 1,399 |
| `bis-core-principles-2012-to-2024` | 11.23% -> 74.93% | 78 -> 1,214 |
| `oasis-mqtt-311-to-50` | 48.86% -> 30.45% | 1,884 -> 1,134 |
| `libreoffice-getting-started-74-to-75` | 36.50% -> 39.42% | 863 -> 2,028 |

This is production-traversal evidence, not a passed quality gate. Schema v36
leaves every comparison field and every complete-scope metric identical to
schema v35. It corrects the SP 800-57 toolkit expectation: the footnote exists
in both revisions, and Rev. 5 removes only the comma before `rather`. The
reviewed replacement counterpart count therefore rises from three of three to
four of four evaluable pairs at candidate recall@32, while global reviewed
recall remains 0.750.

The corrected counterpart is found on page 16 in both revisions, but its
whole-Sentence units occupy spans 32 and 42 and score only 2,347 against each
other. Each side instead has tied best and second relation scores of 10,000,
and the watched pair is neither best nor reciprocal. The next investigation is
a behavior-neutral quote-local fragment or clause-boundary diagnostic;
thresholds and margins remain unchanged. Across all watches, schema v36 records
218 bounded one-sided evidence items in 12 records, 434 observed opponents, and
157 authoritative eligible best opponents. None reaches the production veto
threshold, so these diagnostics do not justify a one-sided behavior change.

The `447927d` schema-v34 capture remains the historical reference-oracle proof.
It preserves candidate keys, posting counts, and ordering while packing exact
FIRST/LAST provenance into each existing posting word. All eight reference
oracles complete within their existing budgets with identical plans and
retained-pair count, set, and order fingerprints.
LibreOffice classifies all 18,853,226 broad pairs with 24,943,792 edge
comparisons under the unchanged 32,000,000 cap, then charges 16,576 downstream
near pairs. Removing the reference-oracle object produces exact `e254b48`
parity for every comparison, quality, candidate-recall, direct-replay, and other
diagnostic field. Posting elements remain one `usize` wide.

The `e254b48` schema-v34 capture made the independent reference oracle apply
the exact Sentence edge gate to candidates from the legacy
`UnitCandidateIndex` before
charging retained pairs to near-relation work. Seven of eight reference
oracles now complete with identical plans and retained-pair count, set, and
order fingerprints. LibreOffice no longer reaches the 8,000,000 near-pair cap:
it charges only 7,993 downstream pairs after classifying 9,596,446 broad pairs.
It instead stops fail-closed at the unchanged 32,000,000 edge-comparison cap,
with 1,831 attempted broad pairs still unclassified. Removing the reference-
oracle object produces exact schema-v33 parity for every comparison, quality,
candidate-recall, direct-replay, and other diagnostic field. The next step is
to reuse the legacy index's exact first/last-edge evidence during reference
classification, rather than increasing this diagnostic budget.
The schema-v33 capture replaces sorted repeated signature entries with compact
unique-key ranges and separate occurrence arrays. All 18 available direct
replays now complete, including LibreOffice, and all retained-pair, candidate-
order, watch-invariant, and watch-preservation mismatch counters remain zero.
Across the 17 replays that already completed in schema v32, aggregate logical
index bytes decrease from 286,117,632 to 96,593,256 (66.24%); every individual
record decreases. LibreOffice completes 1,223,148 postings and 842,929 distinct
keys in 30,017,144 logical bytes, instead of stopping after 1,033,096 postings
at an attempted 69,960,896 bytes. The 17 common records preserve exact parity
for every non-storage direct field. Removing the direct and reference-oracle
objects produces exact schema-v32 parity for all comparison, quality, candidate-
recall, and other diagnostic fields. The only additional diagnostic work is the
LibreOffice reference oracle: direct replay now finishes, so the oracle advances
to its existing pair-visit limit instead of stopping at `direct_replay_incomplete`.
Its plan and fingerprint remain unevaluable.

This result does not yet enable production traversal. The 17 paths with a
complete comparable legacy denominator generate 1,824,541 direct candidates
from 15,157,148 broad pairs (12.04%). The exact retained set itself contains
1,650,014 pairs (10.89%), so the original sub-10% aggregate gate cannot coexist
with exact retained-set parity. The index adds 174,527 candidates above that
minimum, equal to 1.15% of broad work or 10.58% over the exact minimum. The
activation gate must therefore be restated against the exact lower bound, and
the remaining LibreOffice reference safety evidence must be resolved, before
the diagnostic index can replace production candidate generation. Budgets and
relation thresholds remain unchanged.
The schema-v32 capture replaces per-key posting vectors with sorted flat
signature entries while retaining exact contiguous-range lookup. Across the 17
completed direct replays, aggregate logical index bytes decrease from
372,771,728 to 286,117,632 (23.25%), and aggregate posting capacity decreases
from 11,916,680 to 5,108,896 slots. All non-storage direct-replay fields for
those records retain exact schema-v31 parity, including candidate order,
retained fingerprints, and watch evidence. LibreOffice advances from 622,349
to 1,033,096 indexed items and from 1,470 to 4,279 queries before the same typed
estimated-byte stop, but still does not complete: repeated keys in flat entries
remain too expensive under the cumulative recovery allocation ceiling. This
result motivates a compact key-range table with a separate occurrence array;
it does not justify increasing the budget or enabling production traversal.
Removing the schema number and the direct-shadow object yields exact behavior
parity with schema v31.
The schema-v31 capture verifies that direct edge-signature replay preserves
reviewed-change watch evidence without affecting comparison behavior. Seventeen
of 18 available direct replays complete and all 17 preserve the deterministic
watch evidence available from their accepted recovery build. Ten records also
have complete accepted recovery builds and preserve the full watch diagnostics
exactly. Three watched legacy candidates are absent from the signature index:
one in EDPB Right of Access and two in NIST CSF. The bounded direct probe checks
those three pairs with 39 token comparisons, retains their diagnostic-only low
edge scores, and records zero invariant violations or preservation mismatches.
LibreOffice remains fail-closed at the existing estimated logical-byte limit.
Removing the schema number and the 11 direct-watch fields yields exact behavior
parity with schema v30.
The schema-v30 capture gives completed direct replays a comparable full legacy
Sentence edge denominator. The ten production-complete records examine 675,285
broad pairs and generate 56,587 direct candidates. The seven completed
reference oracles examine another 14,481,863 broad pairs and generate 1,767,954
direct candidates. Combined, the direct index generates 1,824,541 candidates
from 15,157,148 broad pairs, or 12.04%. However, 1,650,014 pairs, or 10.89% of
the broad set, actually pass the exact edge gate and therefore form a hard
lower bound for an index required to reproduce the exact edge-gate retained
set. The direct index adds 174,527 candidates above that lower bound: 1.15% of
broad work and 10.58% over the exact minimum. The original sub-10% aggregate
gate is therefore incompatible with the current exact retained-set parity
contract. Production remains on hold until the gate is restated against this
exact lower bound and the bounded watch/fallback contract is verified.
The schema-v29 capture independently checks direct edge-signature results
against a higher-limit broad legacy candidate oracle whenever the accepted
recovery build is incomplete. Seven of eight eligible records complete the
oracle: FIPS 186, SP 800-57, SP 800-171, both EDPB pairs, BIS Core Principles,
and MQTT. All seven preserve the recovery plan and retained-pair fingerprint;
retained-pair miss, count, set, and order mismatch totals are zero. Together,
the completed oracles examine 14,504,373 candidate pairs and perform 51,291,724
similarity comparisons. LibreOffice remains fail-closed because its direct
replay is incomplete, and records `direct_replay_incomplete` instead of a
partial parity claim. Removing the schema number, the reference-oracle object,
and the four nested fragment-veto work counters yields exact behavior parity
with schema v28. This strengthens the candidate-safety evidence for incomplete
production searches. Schema v29 did not expose the completed oracle's broad
edge denominator, so its direct-to-production aggregate must not be used as a
selectivity gate.
The schema-v28 capture sources Sentence candidates directly from the exact
edge-signature index before the broad first/last-token union. Seventeen of 18
available replays complete; LibreOffice stops fail-closed at the estimated
logical-byte limit. The ten replays with complete production baselines preserve
both the recovery plan and retained-pair fingerprint, and every mismatch counter
is zero. Direct replay performs zero broad Sentence posting visits. Across the
17 complete replays, however, 1,824,541 signature candidates remain versus
4,868,816 pairs observed by the accepted production edge filter. The resulting
37.47% was provisional: schema v30 shows that this denominator mixes completed
direct work with partial accepted searches and is not comparable.
Depth-four-or-greater queries account for 98.95% of those candidates;
cross-orientation-only candidates are zero. SP 800-57 reaches 63,820,576 logical
index bytes, while LibreOffice attempts 70,622,544 before stopping. The direct
index therefore remains diagnostic until candidate selectivity or index storage
improves without weakening relation semantics.
The schema-v27 capture independently replays Sentence candidate search through
the exact edge-signature index without changing comparison behavior. Removing
the schema number and the new signature-shadow field yields a byte-identical
canonical JSON representation of the schema-v26 capture. Across 18 available
records, the replay considers 16,208,932 pairs and retains 972,807, pruning
93.998%. Twelve replays complete. All ten replays whose production baseline is
also complete preserve the recovery plan and miss no retained edge-gate pair.
The remaining six replays stop at the existing candidate-posting visit limit
before the signature filter can finish, despite pruning 94.956% of the pairs
observed before those stops. The signature gate therefore remains diagnostic;
the next candidate-search change must avoid constructing the broader Sentence
edge union before consulting this index.
Role-local alignment raises SP 800-57 comparison coverage from 70.95% to 71.90%
and corrects its reviewed kind accuracy from 0.750 to 1.000. NIST CSF coverage
decreases from 53.52% to 53.22% without changing reviewed recall or kind accuracy.
Running-matter boundaries split unresolved evidence more locally, so raw unresolved
region counts are not directly comparable with the preceding capture; token shares
remain the coverage measure.
The schema-v8 diagnostics identify six near-relation searches stopped by the
pair-visit budget and one stopped by the similarity-comparison budget. Ten
complete-extraction pairs finish near-relation analysis, and no pair reaches
the recovery-unit candidate-count cap. Component score upper bounds reduce
similarity-comparison work without changing any report field outside the
sentence-recovery diagnostics compared with the `dfc77f1` capture.
The schema-v9 structural diagnostics succeed for 17 pairs and preserve every
schema-v8 report field from the `4e6ae73` safe baseline. Across the corpus they
observe 609,532 structural profile candidate pairs, of which 609,505 are
duplicates. Only 27 profile pairs are reciprocal singletons, and only one has
monotone exact-anchor evidence. NIST CSF has 1,207 candidates, all duplicate,
so topology alone is not safe evidence for enabling run pairing there.
The schema-v10 run-signature diagnostics also finish for all 17 available pairs
without changing any schema-v9 report field. They observe 110 shared exact-unit
keys, 1,022 candidate run pairs, 38 reciprocal unique pairs, and 22
margin-qualified monotone pairs. Those 22 pairs occur in arXiv Attention,
QGIS PyQGIS English, and LibreOffice Getting Started. NIST CSF has 60 old and
62 new eligible unique units but no shared key, so exact run signatures cannot
recover its two reading-order misses.
The schema-v11 recovery-watch diagnostics preserve every schema-v10 report
field. Eight reviewed pairs expose watch results: six searches are complete,
including two completed-empty query sets, while FIPS and EDPB stop at the
existing pair-visit limit. The 12 watched changes contain 15 found, five
ambiguous, and four unfound side occurrences. Five watched records reach an
existing near comparison, and two are reciprocal. Both NIST CSF misses are
found inside the same unresolved span and examined, but their watched scores
are only 57 and 86; neither counterpart is the best relation, and each old-side
best score is tied at 5,000. Together with the absence of shared exact run
signatures, this rules out simply enabling trusted-run pairing for those
misses. The evidence instead points to recovery-unit granularity and competing
relations. The remaining FIPS move is unfound on both sides, which supports
segment-level move detection rather than a looser run-pairing rule.
The schema-v12 capture preserves every comparison, extraction, quality,
candidate, and sentence-recovery metric from schema v11. Its adjacent-unit
watch locates the remaining FIPS move on both sides as one segment: old page
27 in span 63 and new page 30 in span 75. Unit-level exact counts and near
relations are explicitly unavailable for this diagnostic segment, so it does
not claim a move relation yet. Across the 12 watched changes, the side-local
totals are now 17 found, five ambiguous, and two unfound occurrences; five
records reach an existing near comparison and two are reciprocal. Pair evidence
is omitted for the OASIS CSAF URL and IRS W-4 footer because their found
occurrences have no alignment-span location.

## Current Writer Schema (v64)

The benchmark writer and latest committed capture use schema v64. Older
captures retain their recorded schemas. Schema v64 records bounded exact-range
parent diagnostics over verified recovery ownership. It reports typed atomic
stops, exact token revalidation, uniqueness, overlap and nesting vetoes, and
strong reciprocal Section-parent evidence without changing comparison
behavior. Removing `exact_range_parent_outcome` and the schema number yields
exact parity with schema v63. Schema v63 records bounded proposal
review evidence for schema-v62 changed one-to-one paragraph gaps. Each complete
bundle includes exact edit traces, source provenance, recovery ownership,
existing-change overlap, structural evidence, a full-evidence fingerprint, and
a deliberately strict audit-only gate. A typed stop publishes no partial
proposal data. The original schema-v63 capture was exactly equal to schema v62
after removing `section_pairing_proposal_review_bundle` and the schema number;
the current schema-v63 behavior capture is not. Schema v62 records a bounded, atomic,
behavior-neutral full-document section-pairing shadow gated on the verified
recovery-ownership sidecar and reported alongside the schema-v61
structural-container inventory. Their populations and counters are separate.
The section-pairing shadow separates strong and number-only heading and
paragraph views, validates their parent, topology, membership, anchor, gap, and
work partitions, and does not change comparison behavior. Removing
`section_pairing_shadow` and the schema number yields exact parity with schema
v61. Schema v61 records behavior-neutral, bounded structural-container
diagnostics derived from verified recovery ownership. Removing
`structural_container_shadow` and the schema number yields
exact parity with the schema-v60 capture. Schema v60 records conservative,
anchor-bounded content-presence proofs and keeps them separate from exact
`ChangeEvent` output and unresolved regions. It also reports reviewed content
presence, exact localization, semantic relation, and move recall independently.
Removing the two schema-v60 record fields and the schema number yields exact
parity with the corrected schema-v59 baseline. Schema v59 records bounded paired-stream
sequence paths, path-exclusion margins, downstream vetoes, and disjoint local
near-veto reasons without changing comparison behavior. Schema v58 records bounded, typed
source-range topology evidence for exact adjacent segments without changing
comparison behavior. It distinguishes complete, not-applicable, and stopped
analysis, keeps recovery-unit and alignment-anchor evidence separate, and
publishes no partial pair classification after a budget stop. Schema v57 adds
typed, bounded wrong-kind relation evidence to reviewed failure diagnostics. A scan stop marks
the diagnostic incomplete without failing the benchmark pair or publishing
partial evidence; unavailable source projection remains distinct from zero
changed tokens. Schema v56 records bounded exact
trusted-residual candidate counts, selected matches, completion, a typed stop
reason, and resolved context under the `trusted_residual_exact` origin. The
stage consumes the verified v55 ownership inventory, requires globally unique
full-token equality inside one compatible alignment span and role, and rolls
back atomically if output preparation cannot commit the complete secondary
batch. Schema v55 records source-backed
recovery ownership partitions, per-origin committed event and token totals,
and bounded deterministic review traces for committed local-fragment edits.
Every ownership range is accepted, an unselected leaf, or a typed gap; stopped
analysis publishes no partial partition. The latest capture also uses the
existing exact-match and remainder-attribution fields for isolated range-local
exact recovery; it does not add a schema field. Schema v54 records
behavior-changing, isolated exact-tail recovery: global multiplicity census
work, normal-unit conflicts, exact token verification, typed stops, candidate
classification, and committed matches. Neither isolated exact stage
participates in ordinary anchor or near-relation selection, and incomplete work
publishes no partial result counters. Schema v53 partitions every unresolved
sentence-recovery source token into one mutually exclusive cause when the
bounded analysis completes. Incomplete attribution publishes no partial
counts and records a typed stop reason; diagnostic allocation or invariant
failure never discards the production recovery plan. Schema v52 records production
local-fragment proposal completion, typed stops, considered candidates, and
committed replacements. It promotes only exact interior one-token edits after
the schema-v51 flat exact-boundary analysis completes. Schema v50 preserves the
exact-token certification and retained stream while replacing transition-trie
storage with bounded, depth-local exact classes. Schema v49 preserves the global
length-aware candidate stream and adds a shared exact-token trie that certifies
prefix and suffix comparisons without changing retained-pair behavior. Schema
v48 builds only the global
length-aware fragment candidate stream while preserving fixed-depth parent
admission and exact-recheck parity. Schema v47 measures a length-aware-only
exact-recheck stream under an independent budget and verifies its available
candidate and retained fingerprints against the existing reuse shadow. Schema
v46 attributes threshold-capped local-fragment recheck outcomes and comparisons
to shared, fixed-depth-only, and length-aware-only candidate memberships.
Schema v45 stops exact edge scans
when their monotone score reaches the unchanged threshold. Schema v44 merges
the fixed-depth and length-aware candidate streams so each unique pair is
rechecked once. Schema v43 removes repeated parent identity from global
fragment signature traversal, schema v42 introduces length-aware own/all-depth
signatures, and schema v41 attributes exact edge and word-scoring work. Schema
v40 adds a parent-scoped
boundary-signature index to the optional, annotation-independent local-fragment
shadow. Prefix and suffix signatures are cached once per fragment, kept in
separate edge-side namespaces, and reused by both candidate stages. Hash matches
only generate candidates; exact edge rechecks still decide eligibility. The
schema records boundary index postings, queries, and posting visits separately
from the parent index, with independent typed resource stops. A stopped shadow
exposes only immutable configuration plus examined and attempted work; it never
publishes a partial relation set or changes the comparison plan. Schema v39
introduced fixed-minimum-depth parent signatures and separate parent candidate
counts. Schema v38 introduced the range-backed prefix and suffix fragment model
and its bounded sampled edit evidence. Schema v37 adds bounded
quote-local exact diagnostics to expected-change watches. Schema v36 adds
diagnostic-only one-sided recovery-veto evidence to each expected-change watch.
It records the watched side, relation availability, veto state, best and second scores, the
production-selected eligible opponent and its actual near-search scope, plus
at most two observed opponents, including descriptor-backed geometry and
containment when available.
Same-span, ambiguous-span, cross-span, and paired-stream scopes remain distinct.
If this bounded sidecar cannot be completed, its partial evidence is discarded
without changing the comparison plan. Schema v35 records whether direct
edge-signature work is a diagnostic shadow replay, an accepted production
result, or discarded production work followed by the atomic legacy fallback.
The associated direct metrics are present only with this execution provenance;
accepted production requires a complete direct result without fallback, while
discarded production requires fallback. CLI trace schema v24 exposes the same
three states as one-hot counters. Schema v34 remains the historical
reference-oracle schema: it separates reference-oracle
edge-filter pair and comparison work from downstream near-relation work and
publishes typed edge-filter stops. CLI trace schema v23 exposed the same work
and stop semantics. Schema v33 stores signature postings as
compact unique-key ranges plus occurrence arrays, accounts for temporary and
final transition capacity before every bounded allocation, and preserves typed
progress on allocation failure. CLI trace schema v22 exposes the same storage
and stop semantics. Schema v32 changes signature-index
capacity and logical-byte diagnostics to the sorted flat-entry representation.
It also enforces a tighter independent distinct-key limit through a bounded
preflight only when the posting upper bound cannot already prove the limit.
CLI trace schema v21 exposes the same storage semantics. Schema v31 adds bounded probes for
watched legacy Sentence candidates omitted by direct signature traversal,
typed probe stops and invariant failures, behavior-neutral watch-preservation
evidence, and exact watch parity when the accepted recovery build is complete.
CLI trace schema v20 exposes the same counters, status fields, and one-hot stop
reasons. Schema v30 adds full legacy Sentence
edge attempted, examined, retained, and rejected work to the completed reference
oracle. CLI trace schema v19 exposes the same counters. Schema v29 adds the independent
higher-limit legacy reference oracle for direct edge-signature results, typed
oracle stops, retained-pair fingerprint parity, and fragment-veto work evidence.
CLI trace schema v18 exposes the same metrics and one-hot stop reasons. Schema
v28 adds the independent direct
edge-signature replay, bounded index shape and resource diagnostics, exact-edge
rechecks, cross-orientation evidence, and plan/fingerprint parity. Removing the
schema number and the direct-shadow object, while excluding the SP 800-57 and
MQTT records whose reviewed annotations changed, yields exact behavior parity
with schema v27. CLI trace schema v17 exposes every direct metric and one-hot
stop reason. Schema v27 adds the independent exact
edge-signature candidate replay, its bounded index and query work, typed stops,
candidate reduction, retained-edge verification, and recovery-plan parity.
Removing the schema number and this diagnostic object yields an exact match
with the schema-v26 `ab26b3c` capture. CLI trace schema v16 exposes the same
metrics and one-hot stop reasons. Schema v26 records whether an
incomplete filtered recovery build was discarded and separately reports the
pair, similarity-comparison, and candidate-posting work spent by that discarded
build. These counters never include work adopted by the fallback result.
CLI trace schema v15 exposed the same fallback and discarded-work evidence.
Schema v25 reports the production
Sentence edge filter's bounded pair and comparison work, retained and rejected
pairs, completion state, and typed query-fallback reason. CLI trace schema v14
exposes the same counters and one-hot fallback reasons.
Schema v24 adds a behavior-neutral Sentence edge-gate
shadow that excludes only pairs whose production edge score is below 3,000,
retains Line relations unchanged, records typed stops, and compares veto,
unique-partner, reciprocal, and adopted-replacement decisions without changing
comparison output. Removing the schema number and the shadow object produces
an exact match with the schema-v23 `12fb039` capture. A later completion audit
found that three stage-local shadows were marked complete even though the
overall near relation had stopped; schema v25 corrects that final consistency
check. The nine fully completed relation shadows examine 570,375 Sentence pairs
and reject 546,307 (95.78%) while recording zero threshold violations, veto
mismatches, unique-partner mismatches, reciprocal-pair mismatches, adopted-
replacement mismatches, and insertion/deletion-veto mismatches. The schema-v24
artifact remains unchanged as the original shadow measurement. Production
schema v26 uses the proved score boundary with query-atomic handling, full-build
legacy fallback for incomplete relations, and separate bounded work accounting.
Schema v23 measures a behavior-neutral exact relation-floor probe during
cross-span Sentence word scoring. Removing the four probe counters leaves every
`f0ffe19` field value unchanged. The latest writer retains pair-complete probe
observations when a speculative cross-span search later exhausts its budget,
without retaining the speculative relations. Eleven records commit 1,341,314
probed pairs; 17,715 enter word-multiset scoring, 3,656 reach the safe stop
condition, and stopping there would save 7,660 word comparisons. That is 0.155%
of all 4,946,629 cross-span Sentence similarity comparisons and 0.034% of all
22,564,695 near-search comparisons. The optimization is therefore not enabled.
Schema v22 classifies cross-span Sentence scoring into the same paired anchor
interval, another interval in the same paired stream, page-only locality, and
unclassified topology. It also replays a strict shadow that retains only the
first class. Removing the schema number and the shadow object leaves every
schema-v21 `d045e2c` field value unchanged. Fourteen pairs expose the replay and
ten complete it. Those complete replays score 344,546 cross-span traversals,
but only 138 are in the same paired anchor interval; 259 are in another interval
of the same paired stream, 4,110 have page-only locality, and 340,039 are
unclassified. Only three complete replays preserve exact relation parity;
118 unique-partner and 51 reciprocal-pair decisions change in total. Paired
anchor intervals are therefore too sparse to serve as a strict cross-span
filter, and the result remains diagnostic-only.
Schema v21 replays Sentence relations after excluding ambiguous-span
counterparts from the same-or-ambiguous phase. The replay is diagnostic-only:
removing the schema number and `known_span_sentence_shadow` leaves every
schema-v20 `a7a483c` field value unchanged. Fourteen pairs expose a replay, ten
complete it, and five preserve exact relation parity. Even among complete
replays, four unique-partner decisions and two reciprocal pairs change, so
known-span filtering is not safe to enable. The four budget-stopped replays
retain only 17.57% to 58.88% of considered pairs and all change exact relations;
FIPS, SP 800-57, EDPB Right of Access, and MQTT record 16 unique-partner and
nine reciprocal-pair mismatches in total.
The `d045e2c` writer stops sorted word-multiset scoring once the remaining
overlap cannot exceed the score already established by edge or Line evidence.
All fields outside sentence-recovery diagnostics remain identical to the
`a5202c8` capture. Seventeen pairs avoid 29,782 similarity comparisons in total;
BIS Core Principles still reaches its comparison limit but examines 469,079
candidate pairs instead of 467,736.
Schema v20 divides same-or-ambiguous work into known same-span candidates,
ambiguous-span candidates, and shared query preparation. All 11 child counters
must sum to the schema-v19 parent for Sentence and Line units. Removing the
schema number and the three new child objects leaves every schema-v19
`ba424d8` field value unchanged.
The stopped pairs do not share one dominant cause. Ambiguous Sentence pairs
account for 64.54% of the same-or-ambiguous phase in FIPS, 81.20% in SP 800-57,
82.42% in EDPB Right of Access, and 87.82% in LibreOffice. Known same-span pairs
account for 98.05% in NIST SP 800-171, 100% in EDPB Dark Patterns, and 99.41%
in BIS Core Principles. MQTT is mixed at 58.88% known and 41.11% ambiguous,
while its larger global cost remains cross-span search. Candidate-locality work
must therefore treat ambiguous multi-span units and known span-local edge
collisions as separate problems.
Schema v19 divides each Sentence/Line work counter among paired intervals,
paired cross-interval vetoes, same-or-ambiguous spans, and cross spans. The
writer validates all 11 scope counters against their unit-kind totals, and
posting, pair-visit, and similarity totals against existing aggregates,
including failed atomic charges. Removing the schema number and the four new
scope objects leaves every schema-v18 `496d128` field value unchanged.
Of the eight pairs with incomplete near-relation analysis, six spend most
Sentence pair visits in the same-or-ambiguous phase. EDPB Right of Access and
MQTT instead spend 69.54% and 77.82% in cross-span search. The two paired phases
together stay below 2% for every stopped pair. Candidate-locality work should
therefore focus on separating ambiguous-span work and bounding cross-span work,
not on the paired phases.
Schema v18 attributes edge-posting, Line-trigram posting, query-union,
filtered-candidate, pair-visit, and similarity-comparison work to Sentence and
Line recovery units. The per-kind examined and attempted counters are validated
against the existing aggregate counters, including failed atomic charges.
After removing the schema number and the two new work-attribution objects, all
other field values equal the schema-v17 `64a58b7` capture.
Eight pairs still stop near-relation analysis: seven at the pair-visit limit and
BIS Core Principles at the similarity-comparison limit. Sentence units account
for 99.47% to 100% of attempted pair visits in those pairs, with a mean of
99.87%. This evidence directs the next bounded candidate-locality work toward
Sentence units rather than further Line-index expansion.
Schema v17 records candidate-posting work separately from candidate-pair and
full-similarity work, including examined and attempted visits and a typed
posting-limit stop. Line candidate lookup now combines edge evidence with
role- and kind-local interior trigrams. Trigram postings retain multiplicity,
and only candidates that can meet the existing 7,000-basis-point multiset-Dice
threshold reach full similarity. Paired-stream crossing checks use pair-local
posting buckets, so unrelated trusted-stream pairs are excluded before query
work is charged.
All 29 pairs finish without a resource stop. Ten of the 18 pairs with available
sentence-recovery diagnostics complete near-relation analysis; seven retain the
existing pair-visit stop and BIS Core Principles retains its
similarity-comparison stop. No pair reaches the candidate-posting limit.
Every reviewed recall, kind, and complete-scope precision metric is unchanged
from schema v16. IRS Form 1040 gains one threshold-qualified Line replacement,
raising comparison coverage from 36.80% to 37.83%; the other 28 pairs preserve
their comparison and quality fields.
Schema v16 allows an expected change to require an exact occurrence count.
Every retained occurrence must match the expected quote shape, and the
deterministic maximum-cardinality assignment prevents competing expectations
from claiming the same event. Matching, count-mismatch evidence, and text scans
share explicit visit, byte, edge, and allocation budgets; exhaustion suppresses
the whole quality result instead of publishing partial metrics.
The comparison engine now recovers exact one-sided line units only when their
role is a repeated header or footer, and groups equal repeated running-matter
insertions or deletions into one semantic event with multiple provenance-rich
occurrences. Body lines remain conservative. The EDPB consultation watermark
is recovered as one deletion with all 51 extracted occurrences, raising its
reviewed recall from 0.500 to 0.750. SP 800-57 retains recall and kind accuracy
of 1.000 while its global content-event count falls from 2,763 to 2,440. The
three pre-existing scoped-complete review sets retain their event and token
precision, recall, F1, and span-IoU values. All 29 pairs finish without a
resource stop or matching-budget quality skip.
Schema v15 adds bounded Clause/ListItem recovery-watch diagnostics. Each unit
retains its kind, byte boundaries, comparable-token count, page, role, and
location availability. Per-side best-partner evidence records the partner
index, best and second scores, exactness, reciprocity, and tied-best state;
aggregate fields expose unit and comparison counts, completion, and a typed
stop reason. This evidence is diagnostic-only and does not alter comparison,
coverage, quality, or candidate-recall behavior.
All eight pairs with recovery watches complete granular diagnostics without a
typed stop. NIST CSF exposes nine old-side and 16 new-side Clause/ListItem units
across its two watched replacements. The function-list evidence isolates reciprocal item
relations and preserves tied best relations for the ambiguous final items;
the all-sector evidence isolates two old and seven new units. These diagnostics
do not justify a behavior change. Outside schema version and recovery-watch
evidence, every compact report field is byte-identical to the schema-v14
`b1e54e3` capture.
Schema v14 extends recovery watches to insertions and deletions. Each queried
side records bounded occurrence evidence with explicit complete or truncated
semantics, while an absent quote is reported as `not_queried` instead of being
mistaken for an unfound occurrence. Comparison, quality, and candidate metrics
remain unchanged.
Retained occurrences expose the page, raw trusted-run bounding box, and block
role already available to the diff. Normalized geometry remains deferred until
the neutral document model retains authoritative page bounds.
The capture records 13 one-sided reviewed changes, all without truncation. The
EDPB consultation watermark is present in 51 distinct old-side pages, and every
occurrence is classified as a repeated-footer line; the new side is explicitly
`not_queried`. SP 800-57 similarly exposes 157 distinct-page occurrences for
its repeated revision branding. Outside schema version and recovery-watch
evidence, every compact report field is byte-identical to the schema-v13
`5ffa3e0` capture.
Schema v13 adds bounded exact segment-pair diagnostics to recovery watches. It
records candidate, hash-match, token-verification, uniqueness, monotonicity,
crossing, overlap-veto, and typed stop evidence, plus the exact segment-pair
classification associated with each watched change. These observations remain
diagnostic-only and do not change comparison output.
The FIPS domain-parameter watch is exact and unique across two units on each
side, but has no crossing-anchor evidence and overlaps an existing recovery.
It is therefore classified as `exact_unique_topology_unknown`, not promoted to
a move. Across all 29 pairs, segment diagnostics finish without a typed stop.
The segment implementation itself preserves comparison behavior; this capture
also includes the separately reviewed mixed-scope and extraction changes made
after the preceding immutable schema-v12 artifact.
Schema v12 extends bounded recovery watches to locate exact quotes spanning two
to eight adjacent units in one trusted stream. Segment observations remain
diagnostic-only, mark unit-level exact counts unavailable, and do not change
comparison output.
Schema v11 adds bounded expected-change recovery watches with side-local unit
locations, existing near-candidate scores and relations, reciprocal status,
and typed search completion evidence. It does not run additional candidate or
similarity work.
Schema v10 adds behavior-neutral, budgeted exact-unit signature diagnostics for
trusted runs, including typed stop reasons and conservative reciprocal,
evidence-margin, and monotonicity classifications.
Schema v9 adds behavior-neutral structural trusted-run profile diagnostics.
Schema v8 adds near-relation work counters, candidate-set maxima, an explicit
candidate-count truncation flag, and typed pair-visit, similarity-comparison,
and candidate-count stop reasons to the nested sentence-recovery diagnostics.
Schema v7 adds optional scoped-complete changed-token precision, recall, F1,
span intersection-over-union, and false-positive density while preserving the
unversioned full-report key set.
Schema v6 adds optional scoped-complete event precision, recall, and F1 without
changing legacy record key sets. The earlier
[`2026-08-29.json`](2026-08-29.json) capture remains the schema-v4 baseline
before alignment-span diagnostics and uncertain-span exact/replacement recovery.

The compact summary preserves manifest order and omits runtime, raw preview text, general failure and extraction-issue details, and local paths so identical engine and corpus states serialize deterministically. It retains the stable `resource_limit_failure` and `quality_skipped_reason` fields.

Each record includes:

- identity and execution state: `pair_id`, set, role, scope, status, provenance, extraction, comparison, and applied limits;
- comparison metrics: per-side coverage, unresolved regions and token shares, and reported exact, proven-presence, formatting, and uncertain changes;
- legacy reviewed quality metrics plus separate presence, localization,
  relation, and move recall when annotations are available;
- `expected_change_diagnostics`, including classified miss reasons and a
  distinct `alignment_span_mismatch` reason when a recalled counterpart was
  assigned to separate old/new alignment spans, plus optional recovery-watch
  occurrence, candidate, score, relation, and stop evidence. One-sided watch
  records also expose bounded veto provenance, the authoritative eligible best
  opponent when present, actual near-search scope, and up to two observed
  opponents;
- `candidate_recall` for reviewed replacement counterparts;
- `sentence_recovery_metrics`, including exact matches, near replacements,
  recovered insertions/deletions, vetoes, an exhaustive typed remainder
  attribution when available, structural and exact-signature trusted-run
  evidence, and whether bounded searches completed within their budgets.

`null` means the metric is unavailable for that record, not zero. Incomplete extraction suppresses comparison-wide coverage and quality claims; available per-side coverage may still be retained.

## Historical Captures

- [`2026-09-03-047786f.json`](2026-09-03-047786f.json): schema-v64 exact range-parent diagnostics before relocated clause-run moves and the proven row-order fix.
- [`2026-09-02-5ed948e.json`](2026-09-02-5ed948e.json): schema-v61 structural-container inventory before section-pairing diagnostics.
- [`2026-09-02-f46b9c4.json`](2026-09-02-f46b9c4.json): corrected schema-v59 ECMA annotation baseline before anchor-bounded content-presence proofs.
- [`2026-09-02-06c6a18.json`](2026-09-02-06c6a18.json): schema-v59 paired-stream sequence-relation shadow before the ECMA annotation correction.
- [`2026-09-02-47e699a.json`](2026-09-02-47e699a.json): schema-v57 reviewed wrong-kind matching after requiring source-backed atomic evidence, before exact source-range topology diagnostics.
- [`2026-09-02-61981dc.json`](2026-09-02-61981dc.json): schema-v57 wrong-kind relation tracing before cross-kind evaluation required source-backed atomic evidence.
- [`2026-09-02-0c06365.json`](2026-09-02-0c06365.json): schema-v56 exact trusted-residual recovery before wrong-kind relation tracing.
- [`2026-09-01-8d80c64.json`](2026-09-01-8d80c64.json): schema-v54 isolated exact-tail recovery before lifting the three-proposal production fragment throttle.
- [`2026-09-01-0fe4e85.json`](2026-09-01-0fe4e85.json): schema-v52 trusted-run tail recovery before unresolved-token cause attribution.
- [`2026-09-01-9fcaee6.json`](2026-09-01-9fcaee6.json): schema-v50 recovered replacements emit bounded-Myers atomic spans before exact local-fragment edits were promoted.
- [`2026-09-01-9ce4bfe.json`](2026-09-01-9ce4bfe.json): schema-v50 flat exact-boundary diagnostics before recovered replacements began emitting exact atomic spans.
- [`2026-08-31-87aa0bb.json`](2026-08-31-87aa0bb.json): schema-v49 shared exact-boundary trie before flat exact-class interning.
- [`2026-08-31-4a77cb4.json`](2026-08-31-4a77cb4.json): schema-v48 global length-aware fragment candidates before exact boundary certification.
- [`2026-08-31-70486f2.json`](2026-08-31-70486f2.json): schema-v47 length-aware-only exact rechecks before removing the global fixed-depth fragment candidate stream.
- [`2026-08-31-666c9dd.json`](2026-08-31-666c9dd.json): schema-v46 fragment-candidate membership outcomes before the length-aware-only recheck shadow.
- [`2026-08-31-121bfb1.json`](2026-08-31-121bfb1.json): schema-v45 threshold-capped exact fragment rechecks before candidate-membership outcome attribution.
- [`2026-08-31-2585c57.json`](2026-08-31-2585c57.json): schema-v44 shared exact fragment rechecks before threshold-capped scanning.
- [`2026-08-31-1dbac7f.json`](2026-08-31-1dbac7f.json): schema-v43 global local-fragment traversal before sharing exact rechecks across fixed-depth and length-aware candidates.
- [`2026-08-31-426f0a5.json`](2026-08-31-426f0a5.json): schema-v42 parent-scoped length-aware local-fragment signatures; one non-empty build completes with exact fixed-depth parity.
- [`2026-08-31-7d34d52.json`](2026-08-31-7d34d52.json): schema-v41 exact local-fragment recheck attribution before length-aware signatures.
- [`2026-08-31-40f8e19.json`](2026-08-31-40f8e19.json): schema-v40 fixed-depth boundary indexing before comparison-stage attribution.
- [`2026-08-31-cda68bf.json`](2026-08-31-cda68bf.json): schema-v39 parent-first fixed-depth local-fragment indexing; all 16 non-empty builds stop before publishing relation evidence.
- [`2026-08-31-c4cc1cd.json`](2026-08-31-c4cc1cd.json): schema-v38 global local-fragment indexing; all 16 non-empty builds stop before publishing relation evidence.
- [`2026-08-31-10e921f.json`](2026-08-31-10e921f.json): schema-v37 bounded quote-local exact diagnostics before annotation-independent fragment enumeration.
- [`2026-08-31-85c799d.json`](2026-08-31-85c799d.json): schema-v36 bounded one-sided veto provenance and the corrected SP 800-57 toolkit-footnote replacement annotation; comparison fields and complete-scope metrics remain identical to schema v35.
- [`2026-08-31-08cc0c6.json`](2026-08-31-08cc0c6.json): schema-v35 production Direct traversal; all 18 available builds are accepted and complete. Its apparent SP 800-57 regression triggered review of the incorrect toolkit-footnote deletion expectation.
- [`2026-08-31-447927d.json`](2026-08-31-447927d.json): schema-v34 completed reference-oracle evidence before production Direct activation.
- [`2026-08-30-f0ffe19.json`](2026-08-30-f0ffe19.json): schema-v23 relation-floor probe before retaining pair-complete observations from budget-stopped cross-span searches.
- [`2026-08-30-d6cd66d.json`](2026-08-30-d6cd66d.json): schema-v22 paired-anchor cross-span Sentence locality and strict-shadow diagnostics before relation-floor probing.
- [`2026-08-30-d045e2c.json`](2026-08-30-d045e2c.json): schema-v21 known-span Sentence shadow replay with bounded word-score early termination before cross-span locality classification.
- [`2026-08-30-a5202c8.json`](2026-08-30-a5202c8.json): schema-v21 known-span Sentence shadow replay before bounded word-score early termination.
- [`2026-08-30-a7a483c.json`](2026-08-30-a7a483c.json): schema-v20 known/ambiguous near-search work attribution before known-span shadow replay.
- [`2026-08-30-ba424d8.json`](2026-08-30-ba424d8.json): schema-v19 phase-level near-search attribution before known/ambiguous subdivision.
- [`2026-08-30-496d128.json`](2026-08-30-496d128.json): schema-v18 Sentence/Line near-search work attribution before phase-level attribution.
- [`2026-08-30-64a58b7.json`](2026-08-30-64a58b7.json): schema-v17 Line trigram candidate indexing and posting-work diagnostics before per-kind attribution.
- [`2026-08-30-ebcb49e.json`](2026-08-30-ebcb49e.json): schema-v16 exact occurrence validation and repeated running-matter event grouping before Line trigram candidate indexing.
- [`2026-08-30-2ddbb6b.json`](2026-08-30-2ddbb6b.json): schema-v15 Clause/ListItem recovery-watch diagnostics before repeated running-matter grouping.
- [`2026-08-30-b1e54e3.json`](2026-08-30-b1e54e3.json): schema-v14 one-sided insertion/deletion occurrence evidence.
- [`2026-08-30-5ffa3e0.json`](2026-08-30-5ffa3e0.json): schema-v13 exact adjacent-segment relation diagnostics.
- [`2026-08-29-24a2300.json`](2026-08-29-24a2300.json): schema-v12 adjacent-unit recovery watches and the pre-segment-relation baseline.
- [`2026-08-29-1a0675f.json`](2026-08-29-1a0675f.json): schema-v11 bounded expected-change recovery watches.
- [`2026-08-29-1463932.json`](2026-08-29-1463932.json): schema-v10 exact-unit signature diagnostics for trusted runs.
- [`2026-08-29-5145723.json`](2026-08-29-5145723.json): schema-v9 structural trusted-run profile diagnostics.
- [`2026-08-29-4e6ae73.json`](2026-08-29-4e6ae73.json): safe schema-v8 near-search baseline before structural trusted-run diagnostics.
- [`2026-08-29-dfc77f1.json`](2026-08-29-dfc77f1.json): schema-v8 near-search work counters, candidate-set maxima, and typed stop reasons.
- [`2026-08-29-f5406f3.json`](2026-08-29-f5406f3.json): role-local running-matter alignment and the schema-v7 scoped-precision baseline.
- [`2026-08-29-27f096e.json`](2026-08-29-27f096e.json): scoped-complete event and changed-token precision for three fully reviewed regions.
- [`2026-08-29-556686c.json`](2026-08-29-556686c.json): behavior-preserving multi-occurrence change-model migration.
- [`2026-08-29-1ed7340.json`](2026-08-29-1ed7340.json): fail-closed fragment-completion vetoes and the corrected EDPB review baseline.
- [`2026-08-29-de4ed4c.json`](2026-08-29-de4ed4c.json): grouped mixed character edit runs into word-level replacements.
- [`2026-08-29-3074d95.json`](2026-08-29-3074d95.json): short structural-label anchors for adjacent value moves.
- [`2026-08-29-80def43.json`](2026-08-29-80def43.json): bounded semantic line grouping for short matched regions.
- [`2026-08-29-c2de839.json`](2026-08-29-c2de839.json): exact and reciprocal near-match recovery for punctuation-free uncertain lines.
- [`2026-08-29-adc74e0.json`](2026-08-29-adc74e0.json): refined semantic hunk grouping, candidate diagnostics, and forced-reading-order miss classification.
- [`2026-08-29-0cef530.json`](2026-08-29-0cef530.json): weighted multiset scoring and page-local short-candidate indexing.
- [`2026-08-29-ca6447c.json`](2026-08-29-ca6447c.json): monotonic exact-unit recovery inside anchored trusted runs.
- [`2026-08-29-987020f.json`](2026-08-29-987020f.json): schema-v5 uncertain-span exact and reciprocal replacement recovery capture.
- [`2026-08-28.json`](2026-08-28.json): 12-pair schema-v1 capture at engine commit `a7b56a7`.
- [`2026-08-26.json`](2026-08-26.json): original five-pair `limit_scale_hint` calibration baseline.

Deterministic serialization guarantees identical bytes only for the same engine and corpus state. Future captures are expected to change as parsing, alignment, recovery, and evaluation improve.
