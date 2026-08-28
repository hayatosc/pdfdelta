# Real-World Revision Benchmark Results (2026-08-26)

This directory contains dated, compact machine-readable evaluation summaries for the real-world revision-pair benchmark track.

## Capture Metadata

- **Capture Date**: 2026-08-26
- **Generator / Engine Commit**: [`53ed950`](https://github.com/hayatosc/pdfdelta/commit/53ed950)
- **Environment**:
  - OS: Linux x86_64 (kernel 6.6)
  - Compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
  - Profile: `pdfdelta-bench` release mode (`--release`)
- **Artifact**:
  - File: [`2026-08-26.json`](2026-08-26.json)
  - Size: 5,554 bytes
  - SHA-256: `8277d28fc20d5056fa51c66a46a3f707db5c09c6d42dbf5448553ff6bb8b9ba6`
- **Replication Command**:
  Because atomic publication refuses to overwrite existing files, write the reproduction summary to a temporary path and compare it to the committed artifact:
  ```bash
  # Generate reproduction summary to /tmp
  mise run bench-revisions-release -- \
    --summary-json-output /tmp/pdfdelta-reproduced-summary.json

  # Byte-compare against the committed artifact
  cmp /tmp/pdfdelta-reproduced-summary.json benchmark/realworld/results/2026-08-26.json
  ```
- **Provenance Verification**:
  All cached PDF pairs were verified against [`benchmark/realworld/manifest.tsv`](../manifest.tsv) via:
  ```bash
  mise run bench-revisions-checksums
  ```

## Evaluation Budget Configuration

The evaluation used default manifest scaling (`limit_scale_hint` per pair in `manifest.tsv` without `--limit-scale` override):
- `nist-fips-186-4-to-5`: `limit_scale_hint = 16.0`
- `nist-sp800-57-part1-r4-to-r5`: `limit_scale_hint = 512.0`
- `irs-form-1040-2024-to-2025`: `limit_scale_hint = 1.0`
- `nist-sp800-171-r2-to-r3`: `limit_scale_hint = 1.0`
- `edpb-right-of-access-v1-to-final`: `limit_scale_hint = 64.0`

## Summary Metrics Overview

| Pair ID | Set | Role | Extracted | Compared | Comp. Coverage | Unresolved Regions | Reported Content | Reported Formatting | Reported Uncertain | Recall | Kind Accuracy | Hunks / Matched | Unmatched Tiny |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `nist-fips-186-4-to-5` | dev | standard | Complete | Incomplete | 29.4% | 93 | 1287 | 15 | 571 | 0.429 (3/7) | 1.000 | 429.0 | 567 |
| `nist-sp800-57-part1-r4-to-r5` | dev | standard | Complete | Incomplete | 39.3% | 211 | 4396 | 69 | 2009 | 1.000 (4/4) | 1.000 | 1099.0 | 1770 |
| `irs-form-1040-2024-to-2025` | holdout | stress | Complete | Incomplete | 45.0% | 10 | 95 | 4 | 27 | 1.000 (5/5) | 1.000 | 19.0 | 32 |
| `nist-sp800-171-r2-to-r3` | holdout | standard | Incomplete | Incomplete | N/A | 0 | 0 | 0 | 0 | N/A | N/A | N/A | N/A |
| `edpb-right-of-access-v1-to-final` | holdout | standard | Complete | Incomplete | 57.5% | 120 | 1510 | 9 | 636 | 1.000 (5/5) | 1.000 | 302.0 | 621 |

## Schema v1 Field Interpretation

- `schema_version`: Version number of the compact summary document schema (`1`).
- `records`: Ordered array of revision pair evaluation summaries preserving manifest ordering.
- `pair_id`: Unique identifier from `manifest.tsv`.
- `set`: Benchmark partition (`"dev"` or `"holdout"`).
- `role`: Pair classification (`"standard"` target or `"stress"` test).
- `in_scope`: Boolean indicating whether the document structure is within target scope.
- `status`: Execution status (`"ok"`, `"limit"`, or `"failed"`).
- `provenance_verified`: Boolean indicating byte count and SHA-256 matched manifest entries.
- `compared`: Boolean indicating comparison pipeline was executed.
- `extraction_complete`: Boolean indicating whether both document sides parsed and extracted without fatal or unsupported construct issues; `null` if execution stopped before extraction (e.g. `--checksums-only`).
- `comparison_complete`: Boolean indicating whether alignment and diff achieved 100% comparable-token coverage with zero unresolved regions; `null` if comparison did not run or stopped at a resource limit.
- `limit_scale_used`: Pipeline limit multiplier applied during execution.
- `resource_limit_failure`: String error if the run stopped at an execution resource limit; otherwise `null`.
- `coverage_old`, `coverage_new`: Per-side comparable-token alignment coverage ratios (`0.0` - `1.0`). Each ratio is non-null when that side extracted successfully (even if `comparison_complete` is `false`) and `null` only when that side's extraction was incomplete or execution stopped before summarization.
- `coverage_comparison`: Overall comparable-token alignment coverage ratio, computed as `min(coverage_old, coverage_new)`. Evaluates to `null` if either side ratio is `null`. Consequently, `nist-sp800-171-r2-to-r3` truthfully records `coverage_old = null` (incomplete old-side extraction), `coverage_new = 0.0`, and `coverage_comparison = null`.
- `unresolved_regions`: Count of bounding regions where layout reconstruction could not establish unambiguous reading order; `null` if comparison was not performed.
- `unresolved_old_token_share`, `unresolved_new_token_share`: Fraction of tokens located in unresolved regions; `null` if comparison was not performed.
- `reported_content_changes`: Count of semantic content changes reported (insertions, deletions, replacements, moves); `null` if comparison was not performed. When extraction is incomplete (e.g. `nist-sp800-171-r2-to-r3`), reported diffs are suppressed so the comparison stage computes an exact zero count (`0`).
- `reported_formatting_changes`: Count of formatting-only changes reported; `null` if comparison was not performed (computes `0` when extraction is incomplete).
- `reported_uncertain_changes`: Count of low-confidence changes identified during comparison; `null` if comparison was not performed. When extraction is incomplete (e.g. `nist-sp800-171-r2-to-r3`), reported diffs are suppressed and computed uncertain changes are exactly zero (`0`).
- `quality`: Object containing expected-annotation matching metrics (or `null` when no annotations exist, extraction was incomplete, or comparison was not performed):
  - `annotation`: Dataset annotation level (`"partial"` or `"complete"`).
  - `expected_changes`: Number of reviewed expected changes in the annotation file.
  - `reported_changes`: Total changes reported in the comparison.
  - `recall`: Fraction of reviewed expected changes matched by reported diffs (`0.0` - `1.0`).
  - `precision`: Fraction of reported changes matching expected changes. Evaluates to `null` for partial annotations because precision cannot be computed without exhaustive ground truth.
  - `kind_accuracy`: Fraction of matched changes having matching semantic change kinds.
  - `reported_hunks_per_matched_change`: Average number of fragmented reported diff hunks per matched expected change.
  - `review_hunks_per_expected_change`: Average review hunks per expected change (`null` for partial annotations).
  - `unmatched_tiny_changes`: Count of unmatched 1- or 2-token reported edits.
  - `unresolvable_reported_spans`: Count of reported diff spans falling in unresolved regions.
- `quality_skipped_reason`: String reason why quality metrics were not computed (e.g. incomplete extraction, resource limit stop, or unannotated pair); otherwise `null`.

## Partial Annotation & Precision Boundaries

All existing expected annotations for the benchmark pairs are partial review sets (`annotation: "partial"`). Consequently:
- `precision` and `review_hunks_per_expected_change` are recorded as `null`.
- Quality evaluation measures recall and kind accuracy exclusively against the explicitly annotated review items.
- Fragmentation (`reported_hunks_per_matched_change`) and unmatched tiny edits (`unmatched_tiny_changes`) serve as proxies for alignment quality and noise.

## Runtime & Diagnostics Exclusion

Runtime durations (`runtime_ms`), detailed diagnostic error traces, raw preview text, and local filesystem paths are omitted from the summary JSON. These exclusions ensure that the serialized artifact is byte-for-byte deterministic across identical engine and corpus states.

## Stability & Evolution Notice

Deterministic serialization guarantees that running the **same engine version** against the **same input corpus** will produce identical JSON bytes. It does **not** imply that future engine versions will produce identical metrics. As layout analysis, alignment heuristics, and diff algorithms evolve, future dated captures will record updated benchmark metrics reflecting those changes.
