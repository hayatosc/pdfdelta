# Real-World Revision Benchmark Results

This directory contains immutable, dated, machine-readable summaries for the real-world revision benchmark. Each capture records the engine and corpus state at that date; later captures never overwrite historical metrics.

## Latest Capture

- **Capture date**: 2026-08-29
- **Generator / engine commit**: [`adc74e0`](https://github.com/hayatosc/pdfdelta/commit/adc74e0)
- **Environment**:
  - OS: Linux x86_64 (`6.6.87.2-microsoft-standard-WSL2`)
  - Compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
  - Profile: `pdfdelta-bench` release mode
- **Artifact**:
  - File: [`2026-08-29-adc74e0.json`](2026-08-29-adc74e0.json)
  - Schema: v5
  - Size: 50,200 bytes
  - SHA-256: `05f50643d3ce1acd73e447aeaec593fc57fad62919c9297ffe2a2047ce2ca807`

The capture contains all 29 manifest pairs. Every pair finished with `ok` status: 18 completed extraction, 11 reproduced their documented incomplete-extraction boundaries, and none stopped at a resource limit or failed. Comparison remains incomplete for every pair.

## Reproduction

Atomic publication refuses to overwrite existing files. Generate a summary at a temporary path, verify provenance, and compare it with the committed artifact:

```bash
mise run bench-fetch
mise run bench-revisions-checksums
mise run bench-revisions-release -- \
  --summary-json-output /tmp/pdfdelta-reproduced-summary.json
cmp /tmp/pdfdelta-reproduced-summary.json \
  benchmark/realworld/results/2026-08-29-adc74e0.json
```

The evaluation uses each pair's `limit_scale_hint` from [`manifest.tsv`](../manifest.tsv), without a global `--limit-scale` override.

## Reviewed-Pair Overview

All 11 annotation files in this capture are partial review sets. Recall and kind accuracy therefore apply only to the recorded review items; precision is intentionally unavailable.

| Pair | Set | Role | Coverage | Unresolved | Content | Recall | Kind | Hunks / Matched | Tiny |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | dev | standard | 64.91% | 2,391 | 1,384 | 0.857 | 1.000 | 230.667 | 134 |
| `nist-sp800-57-part1-r4-to-r5` | dev | standard | 71.00% | 5,343 | 2,536 | 1.000 | 0.750 | 634.000 | 167 |
| `irs-form-1040-2024-to-2025` | holdout | stress | 29.33% | 49 | 13 | 0.000 | N/A | N/A | 1 |
| `edpb-right-of-access-v1-to-final` | holdout | standard | 86.38% | 2,077 | 750 | 0.400 | 0.500 | 375.000 | 80 |
| `arxiv-attention-v6-to-v7` | dev | stress | 87.73% | 42 | 0 | 0.000 | N/A | N/A | 0 |
| `w3c-ws-policy-attach-20060927-to-20061102` | dev | standard | 55.86% | 249 | 129 | 0.750 | 1.000 | 43.000 | 27 |
| `ecma-109-ed10-to-ed11` | dev | stress | 73.72% | 348 | 77 | 0.333 | 0.000 | 77.000 | 5 |
| `oasis-csaf-v2-cs01-to-csd02` | holdout | standard | 65.15% | 511 | 480 | 0.000 | N/A | N/A | 94 |
| `irs-w4-korean-2024-to-2025` | holdout | stress | 22.39% | 7 | 87 | 0.000 | N/A | N/A | 29 |
| `bis-operational-risk-2011-to-2021` | dev | standard | 68.12% | 648 | 350 | 1.000 | 1.000 | 87.500 | 0 |
| `nist-csf-v1-1-to-v2-0` | holdout | standard | 53.52% | 788 | 648 | 0.333 | 1.000 | 648.000 | 0 |

The full artifact also records unannotated pairs, extraction boundaries, unresolved token shares, candidate recall, miss diagnostics, and sentence-recovery metrics.

Compared with the earlier same-day `0cef530` capture, reviewed recall, kind
accuracy, and coverage are unchanged. Semantic hunk grouping reduces reported
content changes by 15 for FIPS 186, 20 for SP 800-57, 12 for EDPB Right of
Access, and 8 for CSAF. Across those four reviewed pairs, unmatched one- or
two-token edits fall by 56. NIST CSF reviewed candidate recall rises from 1/2
to 2/2 after adding a bounded document-local candidate union; both remaining
misses are now classified as reading-order unresolved rather than being
attributed to candidate generation. These reductions improve fragmentation
signals but are not precision claims because all current annotations are
partial.

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

- [`2026-08-29-0cef530.json`](2026-08-29-0cef530.json): weighted multiset scoring and page-local short-candidate indexing.
- [`2026-08-29-ca6447c.json`](2026-08-29-ca6447c.json): monotonic exact-unit recovery inside anchored trusted runs.
- [`2026-08-29-987020f.json`](2026-08-29-987020f.json): schema-v5 uncertain-span exact and reciprocal replacement recovery capture.
- [`2026-08-28.json`](2026-08-28.json): 12-pair schema-v1 capture at engine commit `a7b56a7`.
- [`2026-08-26.json`](2026-08-26.json): original five-pair `limit_scale_hint` calibration baseline.

Deterministic serialization guarantees identical bytes only for the same engine and corpus state. Future captures are expected to change as parsing, alignment, recovery, and evaluation improve.
