# Real-World Revision Benchmark Results

This directory contains immutable, dated, machine-readable summaries for the real-world revision benchmark. Each capture records the engine and corpus state at that date; later captures never overwrite historical metrics.

## Latest Capture

- **Capture date**: 2026-08-30
- **Generator / engine commit**: [`b1e54e3`](https://github.com/hayatosc/pdfdelta/commit/b1e54e3)
- **Environment**:
  - OS: Linux x86_64 (`6.6.87.2-microsoft-standard-WSL2`)
  - Compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
  - Profile: `pdfdelta-bench` release mode
- **Artifact**:
  - File: [`2026-08-30-b1e54e3.json`](2026-08-30-b1e54e3.json)
  - Schema: v14
  - Size: 294,568 bytes
  - SHA-256: `37895a9910e63ab7a51171a085c45650f6ede104d9328818d362d420d24766d6`

The capture contains all 29 manifest pairs. Every pair finished with `ok` status: 19 completed extraction, 10 reproduced their documented incomplete-extraction boundaries, and none stopped at a resource limit or failed. Comparison remains incomplete for every pair.

## Reproduction

Atomic publication refuses to overwrite existing files. Generate a summary at a temporary path, verify provenance, and compare it with the committed artifact:

```bash
mise run bench-fetch
mise run bench-revisions-checksums
mise run bench-revisions-release -- \
  --summary-json-output /tmp/pdfdelta-reproduced-summary.json
cmp /tmp/pdfdelta-reproduced-summary.json \
  benchmark/realworld/results/2026-08-30-b1e54e3.json
```

The evaluation uses each pair's `limit_scale_hint` from [`manifest.tsv`](../manifest.tsv), without a global `--limit-scale` override.

## Reviewed-Pair Overview

This capture contains three scoped-complete review sets plus one complete scope
inside the partial FIPS review set. Precision is available only inside declared
complete scopes; recall and kind accuracy for partial sets still apply only to
their recorded review items.

| Pair | Set | Role | Coverage | Unresolved | Content | Recall | Kind | Hunks / Matched | Tiny |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | dev | standard | 65.03% | 2,395 | 1,346 | 0.857 | 1.000 | 224.333 | 107 |
| `nist-sp800-57-part1-r4-to-r5` | dev | standard | 71.90% | 6,038 | 2,763 | 1.000 | 1.000 | 690.750 | 103 |
| `edpb-right-of-access-v1-to-final` | holdout | standard | 86.60% | 2,252 | 697 | 0.500 | 1.000 | 348.500 | 56 |
| `arxiv-attention-v6-to-v7` | dev | stress | 88.38% | 50 | 1 | 1.000 | 1.000 | 1.000 | 0 |
| `w3c-ws-policy-attach-20060927-to-20061102` | dev | standard | 56.62% | 261 | 128 | 1.000 | 1.000 | 1.000 | 0 |
| `ecma-109-ed10-to-ed11` | dev | stress | 73.27% | 369 | 78 | 0.333 | 0.000 | 78.000 | 5 |
| `oasis-csaf-v2-cs01-to-csd02` | holdout | standard | 65.21% | 515 | 474 | 1.000 | 1.000 | 158.000 | 65 |
| `irs-w4-korean-2024-to-2025` | holdout | stress | 22.70% | 9 | 88 | 0.000 | N/A | N/A | 29 |
| `bis-operational-risk-2011-to-2021` | dev | standard | 68.12% | 648 | 350 | 1.000 | 1.000 | 1.000 | 0 |
| `nist-csf-v1-1-to-v2-0` | holdout | standard | 53.22% | 788 | 648 | 0.333 | 1.000 | 648.000 | 0 |

The full artifact also records unannotated pairs, extraction boundaries, unresolved token shares, candidate recall, miss diagnostics, and sentence-recovery metrics.

| Pair with a complete scope | Event P / R / F1 | Token P / R / F1 | Span IoU | FP tokens / 10k unchanged |
|---|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | 1.000 / 0.000 / 0.000 | 0.000 / 0.000 / 0.000 | 0.000 | N/A |
| `arxiv-attention-v6-to-v7` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | N/A |
| `w3c-ws-policy-attach-20060927-to-20061102` | 1.000 / 1.000 / 1.000 | 0.984 / 1.000 / 0.992 | 0.984 | 68.027 |
| `bis-operational-risk-2011-to-2021` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | N/A |

The original arXiv, W3C, and BIS scoped-complete metrics are unchanged from
`27f096e`.
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

## Current Writer Schema (v15)

The benchmark writer uses schema v15. The latest committed capture uses schema
v14, and older captures retain their recorded schemas.
Schema v15 adds bounded Clause/ListItem recovery-watch diagnostics. Each unit
retains its kind, byte boundaries, comparable-token count, page, role, and
location availability. Per-side best-partner evidence records the partner
index, best and second scores, exactness, reciprocity, and tied-best state;
aggregate fields expose unit and comparison counts, completion, and a typed
stop reason. This evidence is diagnostic-only and does not alter comparison,
coverage, quality, or candidate-recall behavior.
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
