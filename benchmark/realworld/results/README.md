# Real-World Revision Benchmark Results

This directory contains immutable, dated, machine-readable summaries for the real-world revision benchmark. Each capture records the engine and corpus state at that date; later captures never overwrite historical metrics.

## Latest Capture

- **Capture date**: 2026-08-29
- **Generator / engine commit**: [`27f096e`](https://github.com/hayatosc/pdfdelta/commit/27f096e)
- **Environment**:
  - OS: Linux x86_64 (`6.6.87.2-microsoft-standard-WSL2`)
  - Compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
  - Profile: `pdfdelta-bench` release mode
- **Artifact**:
  - File: [`2026-08-29-27f096e.json`](2026-08-29-27f096e.json)
  - Schema: v7
  - Size: 49,682 bytes
  - SHA-256: `cc6499b04f238f56add8b7873f02cb0e8d848fc91a46fe9031f132b9a8b978f7`

The capture contains all 29 manifest pairs. Every pair finished with `ok` status: 18 completed extraction, 11 reproduced their documented incomplete-extraction boundaries, and none stopped at a resource limit or failed. Comparison remains incomplete for every pair.

## Reproduction

Atomic publication refuses to overwrite existing files. Generate a summary at a temporary path, verify provenance, and compare it with the committed artifact:

```bash
mise run bench-fetch
mise run bench-revisions-checksums
mise run bench-revisions-release -- \
  --summary-json-output /tmp/pdfdelta-reproduced-summary.json
cmp /tmp/pdfdelta-reproduced-summary.json \
  benchmark/realworld/results/2026-08-29-27f096e.json
```

The evaluation uses each pair's `limit_scale_hint` from [`manifest.tsv`](../manifest.tsv), without a global `--limit-scale` override.

## Reviewed-Pair Overview

This capture contains three scoped-complete review sets and eight partial review
sets. Precision is available only inside the declared complete scopes; recall and
kind accuracy for partial sets still apply only to their recorded review items.

| Pair | Set | Role | Coverage | Unresolved | Content | Recall | Kind | Hunks / Matched | Tiny |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | dev | standard | 65.03% | 2,396 | 1,349 | 0.857 | 1.000 | 224.833 | 107 |
| `nist-sp800-57-part1-r4-to-r5` | dev | standard | 70.95% | 5,387 | 2,441 | 1.000 | 0.750 | 610.250 | 103 |
| `irs-form-1040-2024-to-2025` | holdout | stress | 36.80% | 75 | 19 | 0.000 | N/A | N/A | 1 |
| `edpb-right-of-access-v1-to-final` | holdout | standard | 86.60% | 2,252 | 697 | 0.500 | 1.000 | 348.500 | 56 |
| `arxiv-attention-v6-to-v7` | dev | stress | 88.38% | 50 | 1 | 1.000 | 1.000 | 1.000 | 0 |
| `w3c-ws-policy-attach-20060927-to-20061102` | dev | standard | 56.55% | 265 | 127 | 1.000 | 1.000 | 1.000 | 0 |
| `ecma-109-ed10-to-ed11` | dev | stress | 73.27% | 369 | 78 | 0.333 | 0.000 | 78.000 | 5 |
| `oasis-csaf-v2-cs01-to-csd02` | holdout | standard | 65.20% | 513 | 474 | 1.000 | 1.000 | 158.000 | 65 |
| `irs-w4-korean-2024-to-2025` | holdout | stress | 22.70% | 9 | 88 | 0.000 | N/A | N/A | 29 |
| `bis-operational-risk-2011-to-2021` | dev | standard | 68.12% | 648 | 350 | 1.000 | 1.000 | 1.000 | 0 |
| `nist-csf-v1-1-to-v2-0` | holdout | standard | 53.52% | 788 | 648 | 0.333 | 1.000 | 648.000 | 0 |

The full artifact also records unannotated pairs, extraction boundaries, unresolved token shares, candidate recall, miss diagnostics, and sentence-recovery metrics.

| Scoped-complete pair | Event P / R / F1 | Token P / R / F1 | Span IoU | FP tokens / 10k unchanged |
|---|---:|---:|---:|---:|
| `arxiv-attention-v6-to-v7` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | N/A |
| `w3c-ws-policy-attach-20060927-to-20061102` | 1.000 / 1.000 / 1.000 | 0.984 / 1.000 / 0.992 | 0.984 | 68.027 |
| `bis-operational-risk-2011-to-2021` | 1.000 / 1.000 / 1.000 | 1.000 / 1.000 / 1.000 | 1.000 | N/A |

Comparison coverage, reported change counts, unresolved regions, and recovery
diagnostics are unchanged from the preceding `556686c` capture. The new values
come from the scoped-complete evaluation schema and three fully reviewed regions.
The W3C scope exposes four extra reported changed tokens at otherwise complete
event recall, demonstrating why event and token precision are tracked separately.

## Current Writer Schema (v7)

The benchmark writer uses schema v7; older immutable captures retain their recorded schema.
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
  assigned to separate old/new alignment spans;
- `candidate_recall` for reviewed replacement counterparts;
- `sentence_recovery_metrics`, including exact matches, near replacements, recovered insertions/deletions, vetoes, remainders, and whether near-relation analysis completed within its budget.

`null` means the metric is unavailable for that record, not zero. Incomplete extraction suppresses comparison-wide coverage and quality claims; available per-side coverage may still be retained.

## Historical Captures

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
