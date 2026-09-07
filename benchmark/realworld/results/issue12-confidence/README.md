# Inferred reading-order confidence capture

Captured on 2026-09-06 (Asia/Tokyo). Issue 12 remains open. This change fixes confidence propagation through sentence recovery; it does not recover the two missing CSF replacements.

`pipeline.rs` now applies inferred-order confidence to final source-backed content and formatting events after every exact-diff/recovery path. Any event touching an inferred block on either document side is `Low`. The unresolved-span recovery eligibility rule remains intact. The integration fixture combines an ambiguous two-column page with an inferred vertical-stack page, verifies that atomic sentence recovery reaches the inferred page, and checks the resulting event confidence.

## Source and execution identity

Both snapshots start at the commit recorded in `source-provenance.json`:

- Before: commit plus `baseline.patch`, retaining the existing two-region layout change.
- After: commit plus `candidate.patch`, adding final confidence propagation and its fixture.

The provenance file records SHA-256 for each Rust production source, Cargo manifest, and lockfile in both snapshots. The patches also retain the test changes. These are uncommitted worktree measurements, not measurements of the commit alone.

The snapshots were built offline in release mode with separate Cargo target directories. `before/run.json` and `after/run.json` identify the actual binaries by SHA-256 and record the command template. The benchmark uses its default manifest-driven resource scales; each record retains `limit_scale_used`. Source snapshots and build outputs are disposable; patches, hashes, and captures are retained here.

`run-pairs.py BINARY OUTPUT_DIRECTORY`, run from the repository root, captures all manifest pairs with two workers. It refuses a nonempty output directory. Per-pair failures are retained when the benchmark writes a valid summary. `before/all.json` and `after/all.json` contain every raw summary indexed by pair ID.

Run `python3 benchmark/realworld/results/issue12-confidence/compare.py` to compare every recorded field. The script requires all 29 pairs and allows only the intended `reported_uncertain_changes` difference. It writes absolute old/new coverage for every pair and capture hashes to `comparison.json`. Missing and null metrics are not evidence of precision or false-positive performance. Resource-limit failures do not count as successful comparisons.

## Results

All 29 before/after summaries match field-for-field except `reported_uncertain_changes`, which increases on eight pairs. Absolute per-pair coverage and the complete list of differences are in `comparison.json`. Available recall, precision, and false-positive metrics remain unchanged.

24 pairs complete comparison execution. Five pairs reach the same resource limit before and after: `qgis-pyqgis-en-328-to-334`, `qgis-doc-guidelines-es-328-to-334`, `libreoffice-getting-started-74-to-75`, `gcc-13-3-to-13-4`, and `unicode-standard-15-to-16`. These failures and unavailable quality metrics remain limitations.

The merged summaries were checked for value equality with every per-pair capture before removing duplicate intermediate JSON files. Per-pair logs and executable hashes remain alongside the merged captures. `review.json` records a separate code review with no confirmed findings.

## Verification

`checks.json` records the commands, exit codes, log hashes, and summaries for workspace tests, Clippy with warnings denied, formatting, and the generated acceptance benchmark. All 48 acceptance cases pass, with zero false-positive changed tokens. The workspace run includes `inferred_order_remains_low_confidence_through_sentence_recovery`.

## Remaining CSF limitation

Both expected replacements, `all-sector-scope-emphasized` and `core-expanded-from-five-to-six-functions`, remain `reading_order_unresolved`. Coverage remains 0.6762929693420874 old / 0.7145010463465675 new; reviewed semantic relation recall remains 1/3.

The retained `expected_change_diagnostics` report no exact shared recovery units for either target. Existing structural and exact-range candidates do not produce unique corresponding boundaries. Quote-local near scores (57 and 431 basis points) are below the existing 7000 acceptance threshold. These observations explain why the current recovery paths cannot establish the two replacements; they do not prove that every possible future matching method would fail.

The diagnostic `fully_contained` flag combines structural eligibility with recovery-span membership. A false value alone does not establish truncated text or membership outside the unresolved window. The known page-wide window therefore cannot be narrowed by interpreting that flag as a text-boundary error.

No recovery threshold was loosened, no document-specific correspondence was hardcoded, and the remaining failures were not reclassified to satisfy the acceptance criterion. Resolving them requires additional, independently checkable correspondence evidence.
