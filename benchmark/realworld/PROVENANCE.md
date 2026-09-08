# Benchmark provenance audit

This audit records which revision pairs belong in development or holdout after
reviewing the checked-in Issue 12 evidence. It does not measure comparison
quality.

The manifest contains 29 pairs: 19 development pairs and 10 holdout pairs.
Five pairs were moved from holdout to development and marked `used_for_fix`
because their source-reviewed scopes and final ownership diagnostics are part
of the checked-in implementation work:

| Pair | Evidence |
| --- | --- |
| `irs-form-1040-2024-to-2025` | [`source-review.json`](results/issue12-anchor-recovery/six-pair-scope-review/irs-form-1040-2024-to-2025/source-review.json), [`final-ownership/source.json`](results/issue12-anchor-recovery/final-ownership/source.json) |
| `edpb-right-of-access-v1-to-final` | [`source-review.json`](results/issue12-anchor-recovery/six-pair-scope-review/edpb-right-of-access-v1-to-final/source-review.json), [`final-ownership/source.json`](results/issue12-anchor-recovery/final-ownership/source.json) |
| `oasis-csaf-v2-cs01-to-csd02` | [`source-review.json`](results/issue12-anchor-recovery/six-pair-scope-review/oasis-csaf-v2-cs01-to-csd02/source-review.json), [`final-ownership/source.json`](results/issue12-anchor-recovery/final-ownership/source.json) |
| `irs-w4-korean-2024-to-2025` | [`source-review.json`](results/issue12-anchor-recovery/six-pair-scope-review/irs-w4-korean-2024-to-2025/source-review.json), [`final-ownership/source.json`](results/issue12-anchor-recovery/final-ownership/source.json) |
| `nist-csf-v1-1-to-v2-0` | [`source-review.json`](results/issue12-anchor-recovery/six-pair-scope-review/nist-csf-v1-1-to-v2-0/source-review.json), [`final-ownership/source.json`](results/issue12-anchor-recovery/final-ownership/source.json) |

The QGIS English and Japanese pairs use the same PyQGIS Developer Cookbook
template and the same 3.28-to-3.34 revision lineage; only the language path
differs. Both therefore use
`qgis-pyqgis-developer-cookbook-328-to-334` for `document_series_id` and
`derivation_group`, and both are in development. The Japanese pair remains
`unused`: the split change prevents translation leakage, while the audit found
no evidence that it informed a fix.

The remaining holdout pairs are `nist-sp800-171-r2-to-r3`,
`hmrc-sa100-2023-to-2024`, `edpb-dark-patterns-v1-to-v2`,
`kicad-getting-started-7-to-8`, `qgis-doc-guidelines-es-328-to-334`,
`oecd-corporate-governance-2015-to-2023`, `libreoffice-getting-started-74-to-75`,
`postgresql-16-to-17`, `nasa-systems-engineering-handbook-rev1-to-rev2`, and
`unicode-standard-15-to-16`. No checked-in targeted-fix evidence identifies
any of these ten series as having informed an implementation choice, so they
remain evaluation-only holdout data.

The final ownership capture covers all 29 pairs, while the six-pair scope
review provides the independently reviewed ranges used for the five named
pairs. Historical captures that label these pairs as holdout predate this
audit and are retained as historical artifacts; they do not define the
current split.
