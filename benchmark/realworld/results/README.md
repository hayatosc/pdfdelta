# Real-World Revision Benchmark Results

This directory contains immutable, dated, machine-readable summaries for the real-world revision benchmark. Each capture records the engine and corpus state at that date; later captures never overwrite historical metrics.

## Latest Capture

- **Capture date**: 2026-08-29
- **Generator / engine commit**: [`3074d95`](https://github.com/hayatosc/pdfdelta/commit/3074d95)
- **Environment**:
  - OS: Linux x86_64 (`6.6.87.2-microsoft-standard-WSL2`)
  - Compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
  - Profile: `pdfdelta-bench` release mode
- **Artifact**:
  - File: [`2026-08-29-3074d95.json`](2026-08-29-3074d95.json)
  - Schema: v5
  - Size: 49,553 bytes
  - SHA-256: `a411fd2a41ea039b0fc8265137302482bf9ff48669e1ec5d6c96602e086f0b29`

The capture contains all 29 manifest pairs. Every pair finished with `ok` status: 18 completed extraction, 11 reproduced their documented incomplete-extraction boundaries, and none stopped at a resource limit or failed. Comparison remains incomplete for every pair.

## Reproduction

Atomic publication refuses to overwrite existing files. Generate a summary at a temporary path, verify provenance, and compare it with the committed artifact:

```bash
mise run bench-fetch
mise run bench-revisions-checksums
mise run bench-revisions-release -- \
  --summary-json-output /tmp/pdfdelta-reproduced-summary.json
cmp /tmp/pdfdelta-reproduced-summary.json \
  benchmark/realworld/results/2026-08-29-3074d95.json
```

The evaluation uses each pair's `limit_scale_hint` from [`manifest.tsv`](../manifest.tsv), without a global `--limit-scale` override.

## Reviewed-Pair Overview

All 11 annotation files in this capture are partial review sets. Recall and kind accuracy therefore apply only to the recorded review items; precision is intentionally unavailable.

| Pair | Set | Role | Coverage | Unresolved | Content | Recall | Kind | Hunks / Matched | Tiny |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | dev | standard | 65.07% | 2,398 | 1,377 | 0.857 | 1.000 | 229.500 | 122 |
| `nist-sp800-57-part1-r4-to-r5` | dev | standard | 71.22% | 5,401 | 2,540 | 1.000 | 0.750 | 635.000 | 164 |
| `irs-form-1040-2024-to-2025` | holdout | stress | 36.80% | 75 | 19 | 0.000 | N/A | N/A | 1 |
| `edpb-right-of-access-v1-to-final` | holdout | standard | 87.12% | 2,262 | 751 | 0.400 | 0.500 | 375.500 | 80 |
| `arxiv-attention-v6-to-v7` | dev | stress | 88.38% | 50 | 1 | 1.000 | 1.000 | 1.000 | 0 |
| `w3c-ws-policy-attach-20060927-to-20061102` | dev | standard | 56.64% | 267 | 131 | 0.750 | 1.000 | 43.667 | 27 |
| `ecma-109-ed10-to-ed11` | dev | stress | 74.02% | 372 | 80 | 0.333 | 0.000 | 80.000 | 5 |
| `oasis-csaf-v2-cs01-to-csd02` | holdout | standard | 65.20% | 513 | 484 | 1.000 | 1.000 | 161.333 | 71 |
| `irs-w4-korean-2024-to-2025` | holdout | stress | 22.70% | 9 | 88 | 0.000 | N/A | N/A | 29 |
| `bis-operational-risk-2011-to-2021` | dev | standard | 68.12% | 648 | 350 | 1.000 | 1.000 | 87.500 | 0 |
| `nist-csf-v1-1-to-v2-0` | holdout | standard | 53.52% | 788 | 648 | 0.333 | 1.000 | 648.000 | 0 |

The full artifact also records unannotated pairs, extraction boundaries, unresolved token shares, candidate recall, miss diagnostics, and sentence-recovery metrics.

Compared with the earlier same-day `80def43` capture, short structural-label
anchors keep `Previous stage:` stable while promoting the adjacent exact URL to
a move. OASIS recall rises from 0.667 to 1.000, kind accuracy remains 1.000,
hunks per matched change fall from 240.000 to 161.333, and tiny unmatched
changes fall from 92 to 71. The OASIS record reports four additional content
changes and 23 fewer uncertain changes. Every other record is byte-for-byte
unchanged. These fragmentation reductions are not precision claims because all
current annotations are partial.

## Current Writer Schema (v5)

The benchmark writer and latest immutable capture use schema v5. The earlier
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

- [`2026-08-29-80def43.json`](2026-08-29-80def43.json): bounded semantic line grouping for short matched regions.
- [`2026-08-29-c2de839.json`](2026-08-29-c2de839.json): exact and reciprocal near-match recovery for punctuation-free uncertain lines.
- [`2026-08-29-adc74e0.json`](2026-08-29-adc74e0.json): refined semantic hunk grouping, candidate diagnostics, and forced-reading-order miss classification.
- [`2026-08-29-0cef530.json`](2026-08-29-0cef530.json): weighted multiset scoring and page-local short-candidate indexing.
- [`2026-08-29-ca6447c.json`](2026-08-29-ca6447c.json): monotonic exact-unit recovery inside anchored trusted runs.
- [`2026-08-29-987020f.json`](2026-08-29-987020f.json): schema-v5 uncertain-span exact and reciprocal replacement recovery capture.
- [`2026-08-28.json`](2026-08-28.json): 12-pair schema-v1 capture at engine commit `a7b56a7`.
- [`2026-08-26.json`](2026-08-26.json): original five-pair `limit_scale_hint` calibration baseline.

Deterministic serialization guarantees identical bytes only for the same engine and corpus state. Future captures are expected to change as parsing, alignment, recovery, and evaluation improve.
