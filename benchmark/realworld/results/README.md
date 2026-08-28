# Real-World Revision Benchmark Results

This directory contains immutable, dated, machine-readable summaries for the real-world revision benchmark. Each capture records the engine and corpus state at that date; later captures never overwrite historical metrics.

## Latest Capture

- **Capture date**: 2026-08-29
- **Generator / engine commit**: [`ffd7600`](https://github.com/hayatosc/pdfdelta/commit/ffd7600)
- **Environment**:
  - OS: Linux x86_64 (`6.6.87.2-microsoft-standard-WSL2`)
  - Compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
  - Profile: `pdfdelta-bench` release mode
- **Artifact**:
  - File: [`2026-08-29.json`](2026-08-29.json)
  - Schema: v4
  - Size: 50,441 bytes
  - SHA-256: `a78bf55286a20014e734842da734f3f3d588033ee9389aca67a1d5936c6e94ea`

The capture contains all 29 manifest pairs. Every pair finished with `ok` status: 18 completed extraction, 11 reproduced their documented incomplete-extraction boundaries, and none stopped at a resource limit or failed. Comparison remains incomplete for every pair.

## Reproduction

Atomic publication refuses to overwrite existing files. Generate a summary at a temporary path, verify provenance, and compare it with the committed artifact:

```bash
mise run bench-fetch
mise run bench-revisions-checksums
mise run bench-revisions-release -- \
  --summary-json-output /tmp/pdfdelta-reproduced-summary.json
cmp /tmp/pdfdelta-reproduced-summary.json \
  benchmark/realworld/results/2026-08-29.json
```

The evaluation uses each pair's `limit_scale_hint` from [`manifest.tsv`](../manifest.tsv), without a global `--limit-scale` override.

## Reviewed-Pair Overview

All 11 annotation files in this capture are partial review sets. Recall and kind accuracy therefore apply only to the recorded review items; precision is intentionally unavailable.

| Pair | Set | Role | Coverage | Unresolved | Content | Recall | Kind | Hunks / Matched | Tiny |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `nist-fips-186-4-to-5` | dev | standard | 64.73% | 2,333 | 1,475 | 0.857 | 1.000 | 245.833 | 157 |
| `nist-sp800-57-part1-r4-to-r5` | dev | standard | 71.73% | 5,356 | 2,884 | 1.000 | 0.750 | 721.000 | 189 |
| `irs-form-1040-2024-to-2025` | holdout | stress | 33.26% | 56 | 19 | 0.000 | N/A | N/A | 1 |
| `edpb-right-of-access-v1-to-final` | holdout | standard | 85.76% | 2,052 | 831 | 0.400 | 0.500 | 415.500 | 88 |
| `arxiv-attention-v6-to-v7` | dev | stress | 87.73% | 42 | 0 | 0.000 | N/A | N/A | 0 |
| `w3c-ws-policy-attach-20060927-to-20061102` | dev | standard | 58.23% | 256 | 156 | 0.750 | 1.000 | 52.000 | 27 |
| `ecma-109-ed10-to-ed11` | dev | stress | 74.17% | 349 | 82 | 0.333 | 0.000 | 82.000 | 5 |
| `oasis-csaf-v2-cs01-to-csd02` | holdout | standard | 70.16% | 611 | 610 | 0.000 | N/A | N/A | 103 |
| `irs-w4-korean-2024-to-2025` | holdout | stress | 22.39% | 7 | 87 | 0.000 | N/A | N/A | 29 |
| `bis-operational-risk-2011-to-2021` | dev | standard | 74.24% | 664 | 433 | 0.750 | 1.000 | 144.333 | 0 |
| `nist-csf-v1-1-to-v2-0` | holdout | standard | 53.63% | 792 | 656 | 0.333 | 1.000 | 656.000 | 0 |

The full artifact also records unannotated pairs, extraction boundaries, unresolved token shares, candidate recall, miss diagnostics, and sentence-recovery metrics.

## Schema v4

The compact summary preserves manifest order and omits runtime, raw preview text, general failure and extraction-issue details, and local paths so identical engine and corpus states serialize deterministically. It retains the stable `resource_limit_failure` and `quality_skipped_reason` fields.

Each record includes:

- identity and execution state: `pair_id`, set, role, scope, status, provenance, extraction, comparison, and applied limits;
- comparison metrics: per-side coverage, unresolved regions and token shares, and reported content, formatting, and uncertain changes;
- reviewed quality metrics when annotations are available;
- `expected_change_diagnostics`, including classified miss reasons;
- `candidate_recall` for reviewed replacement counterparts;
- `sentence_recovery_metrics`, including exact matches, near replacements, recovered insertions/deletions, vetoes, remainders, and whether near-relation analysis completed within its budget.

`null` means the metric is unavailable for that record, not zero. Incomplete extraction suppresses comparison-wide coverage and quality claims; available per-side coverage may still be retained.

## Historical Captures

- [`2026-08-28.json`](2026-08-28.json): 12-pair schema-v1 capture at engine commit `a7b56a7`.
- [`2026-08-26.json`](2026-08-26.json): original five-pair `limit_scale_hint` calibration baseline.

Deterministic serialization guarantees identical bytes only for the same engine and corpus state. Future captures are expected to change as parsing, alignment, recovery, and evaluation improve.
