# Real-World Revision Benchmark Results

This directory contains immutable, dated, machine-readable summaries for the real-world revision benchmark. Each capture records the engine and corpus state at that date; later captures never overwrite historical metrics.

## Latest Capture

- **Capture date**: 2026-08-31
- **Generator / engine commit**: [`08cc0c6`](https://github.com/hayatosc/pdfdelta/commit/08cc0c6)
- **Environment**:
  - OS: Linux x86_64 (`6.6.87.2-microsoft-standard-WSL2`)
  - Compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
  - Profile: `pdfdelta-bench` release mode
- **Artifact**:
  - File: [`2026-08-31-08cc0c6.json`](2026-08-31-08cc0c6.json)
  - Schema: v35
  - Size: 633,643 bytes
  - SHA-256: `8c157d449fc6f80e4e1b13e300c46e9385c00d3580702ae94a1e2a77847dc988`

The capture contains all 29 manifest pairs. Every pair finished with `ok` status: 19 completed extraction, 10 reproduced their documented incomplete-extraction boundaries, and none stopped at a resource limit or failed. Comparison remains incomplete for every pair.

## Reproduction

From a clean checkout of the linked generator commit, generate a summary at a temporary path, verify provenance, and compare it with the committed artifact. Atomic publication refuses to overwrite existing files:

```bash
mise run bench-fetch
mise run bench-revisions-capture -- /tmp/pdfdelta-reproduced-summary.json
mise run bench-revisions-exact-parity -- \
  /tmp/pdfdelta-reproduced-summary.json \
  benchmark/realworld/results/2026-08-31-08cc0c6.json
```

The evaluation uses each pair's `limit_scale_hint` from [`manifest.tsv`](../manifest.tsv), without a global `--limit-scale` override.

When a capture only adds sentence-recovery diagnostics, compare it with the
preceding schema through the bounded parity task instead of maintaining an
ad-hoc `jq` filter:

```bash
mise run bench-revisions-schema-parity -- \
  benchmark/realworld/results/2026-08-31-e254b48.json \
  benchmark/realworld/results/2026-08-31-447927d.json \
  --ignore-field sentence_edge_signature_reference_oracle
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
| `nist-fips-186-4-to-5` | dev | standard | 51.20% | 2,014 | 1,003 | 0.857 | 1.000 | 167.167 | 107 |
| `nist-sp800-57-part1-r4-to-r5` | dev | standard | 52.72% | 5,108 | 1,566 | 0.750 | 1.000 | 314.833 | 103 |
| `irs-form-1040-2024-to-2025` | holdout | stress | 37.83% | 75 | 20 | 0.000 | N/A | N/A | 1 |
| `edpb-right-of-access-v1-to-final` | holdout | standard | 74.81% | 2,091 | 477 | 0.750 | 1.000 | 175.667 | 56 |
| `arxiv-attention-v6-to-v7` | dev | stress | 88.38% | 50 | 1 | 1.000 | 1.000 | 1.000 | 0 |
| `w3c-ws-policy-attach-20060927-to-20061102` | dev | standard | 56.81% | 266 | 130 | 1.000 | 1.000 | 1.000 | 0 |
| `ecma-109-ed10-to-ed11` | dev | stress | 73.27% | 369 | 78 | 0.333 | 0.000 | 78.000 | 5 |
| `oasis-csaf-v2-cs01-to-csd02` | holdout | standard | 65.21% | 515 | 474 | 1.000 | 1.000 | 158.000 | 65 |
| `irs-w4-korean-2024-to-2025` | holdout | stress | 22.70% | 9 | 88 | 0.000 | N/A | N/A | 29 |
| `bis-operational-risk-2011-to-2021` | dev | standard | 68.12% | 656 | 351 | 1.000 | 1.000 | 1.000 | 0 |
| `oasis-mqtt-311-to-50` | dev | standard | 30.45% | 2,307 | 1,134 | 1.000 | N/A | N/A | 0 |
| `nist-csf-v1-1-to-v2-0` | holdout | standard | 53.22% | 788 | 648 | 0.333 | 1.000 | 648.000 | 0 |

The full artifact also records unannotated pairs, extraction boundaries, unresolved token shares, candidate recall, miss diagnostics, and sentence-recovery metrics.

| Pair with a complete scope | Event P / R / F1 | Token P / R / F1 | Span IoU | FP tokens / 10k unchanged |
|---|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | 1.000 / 0.000 / 0.000 | 0.000 / 0.000 / 0.000 | 0.000 | N/A |
| `nist-sp800-57-part1-r4-to-r5` | 1.000 / 0.750 / 0.857 | 0.092 / 0.664 / 0.162 | 0.088 | 6,372.166 |
| `arxiv-attention-v6-to-v7` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | N/A |
| `w3c-ws-policy-attach-20060927-to-20061102` | 1.000 / 1.000 / 1.000 | 0.984 / 1.000 / 0.992 | 0.984 | 68.027 |
| `bis-operational-risk-2011-to-2021` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | N/A |
| `oasis-mqtt-311-to-50` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | 0.000 |

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

This is production-traversal evidence, not a passed quality gate. SP 800-57
global reviewed recall regresses from 0.875 to 0.750 because
`toolkit-footnote-removed` becomes reading-order unresolved. Its scoped event
precision improves from 0.600 to 1.000, while scoped token recall falls from
0.824 to 0.664. The next investigation is one-sided veto diagnostics; thresholds
and margins remain unchanged.

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

## Current Writer Schema (v35)

The benchmark writer and latest committed capture use schema v35. Older
captures retain their recorded schemas. Schema v35 records whether direct
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
- comparison metrics: per-side coverage, unresolved regions and token shares, and reported content, formatting, and uncertain changes;
- reviewed quality metrics when annotations are available;
- `expected_change_diagnostics`, including classified miss reasons and a
  distinct `alignment_span_mismatch` reason when a recalled counterpart was
  assigned to separate old/new alignment spans, plus optional recovery-watch
  occurrence, candidate, score, relation, and stop evidence;
- `candidate_recall` for reviewed replacement counterparts;
- `sentence_recovery_metrics`, including exact matches, near replacements, recovered insertions/deletions, vetoes, remainders, structural and exact-signature trusted-run evidence, and whether bounded searches completed within their budgets.

`null` means the metric is unavailable for that record, not zero. Incomplete extraction suppresses comparison-wide coverage and quality claims; available per-side coverage may still be retained.

## Historical Captures

- [`2026-08-31-08cc0c6.json`](2026-08-31-08cc0c6.json): schema-v35 production Direct traversal; all 18 available builds are accepted and complete, while SP 800-57 exposes a reviewed-recall regression that keeps this capture below the quality gate.
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
