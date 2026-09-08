# Claim-based comparison evaluation

This evaluation preserves the original 29-pair manifest and its 39 evaluable
reviewed expectations. Objective incompatibility is a failure diagnosis, not
an exemption from that denominator. Non-owning review claims, tentative
candidates, exact events, source-mask quality, and comparison completeness
remain separate measurements.

Scoped source-mask quality continues to score accepted events against the
fixed annotations. Review-unit counts are conditional proof evidence, not
additional author-intent recall. Programmatic fixtures establish the new
arithmetic and source projection; they do not establish author-intent
precision for every real-PDF claim.

The implementation adds bounded literal-minimal queries for changed-token
counts and mandatory positions. Source-backed ambiguous line-end hyphens keep
both interpretations; a normalization claim is published only after all
old/new combinations complete. The existing exclusive source partitions and
event ownership are not increased by these claims.

The new literal claim masks preserve the unchanged space in `. F` to `; f`
and the unchanged suffix in `Standard` to `standard`. Legacy semantic event
grouping remains unchanged and can still include that shared space; the
structure probe records this production limitation separately from the new
literal proof.

## Original 29 pairs

The local comparison ledger retains every pair and all 40 reviewed
expectations. The original measurable
denominator remains 39; the BIS opening-sentence scope is still indeterminate.

| Measurement | Frozen baseline | This implementation |
| --- | ---: | ---: |
| Exact expected-event matches, official matcher | 2 / 39 | 2 / 39 |
| False-positive tokens in evaluated scopes | 2 | 2 |
| Accepted events across all pairs | 2,535 | 2,535 |
| Tentative events | 14,340 | 14,340 |
| Complete comparisons | 0 / 29 | 0 / 29 |
| Resource-limit outcomes | 9 | 9 |
| Unsupported extraction outcomes | 8 | 8 |
| Additional unresolved outcome | 1 | 1 |
| Assessment work, 28 available records | 8,485,789,293 | 11,498,569,106 |
| Sum of per-pair comparison time | 2,348.679 s | 1,983.344 s |
| Maximum benchmark-recorded peak memory | 6,128,508,928 B | 6,114,947,072 B |

Every pair retains its previous accepted/candidate counts, coverage, quality,
source-mask measurements, extraction status, and comparison status. The eight
unsupported pairs remain in the ledger; the Japanese QGIS unresolved pair
also records incomplete extraction, making nine extraction-incomplete records
in total. Global precision is unavailable outside the evaluated scopes.
These single-run time measurements do not establish a speed improvement.
All manifest limit scales remain unchanged; work is charged for the new
claims, and missing assessment data for the JVM pair remains unavailable.

The available 28 assessment records contain 8,150 non-owning review units:
8,147 complete and three incomplete. Of these, 1,554 units have a positive
changed-token lower bound. They contain 6,030 old and 9,998 new mandatory
spans, which can overlap between units and must not be counted as additional
owned-token recall. No normalization-hypothesis unit was established in this
corpus; normalization evidence comes from the programmatic regression tests.

The official matcher establishes the aggregate 2/39 score. Per-ID diagnostics
are incomplete for the W3C pair: its publication-date attribution is recorded
as an inference from the unmasked unique quote, separately from the official
aggregate and the complete NIST punctuation diagnostic.

The objective audit keeps the fixed CSF mask at cost 178 against a literal
minimum of 158, and the Attention mask at 11 against 7. Both remain failures
under the fixed acceptance criteria. The supplied-role probe groups the
Attention fields but leaves its mask unchanged; the tested CSF role policy
introduces false positives. Neither experiment enables automatic discovery.

## Generated PDF matrix

The generated benchmark covers all 48 renderer cells. Strict
author-intent acceptance is **42/48**; the other six cells satisfy their
explicit candidate-policy expectations. Changed-token totals are TP 810,
FP 0, and FN 48. The five required core cases pass through both renderers:
line-wrap invariance, page-break invariance, one text replacement, one
paragraph insertion, and one paragraph deletion. The six candidate outcomes
do not redefine the release criteria.

## Reproduction

Formatting, workspace Clippy, all 2,111 workspace tests, and the supplied-role
diagnostic test and executable pass. The diagnostic executable reports the known production
punctuation-mask failure as data; its successful exit does not hide that
failure or establish production readiness.

Build the benchmark binary with stable Rust:

```sh
cargo build --release -p pdfdelta-bench --bin pdfbench
target/release/pdfbench verify
PYTHON_UV=0 python benchmark/realworld/run_claim_evaluation.py NEW_OUTPUT \
  --cache-dir PDF_CACHE
PYTHON_UV=0 python benchmark/realworld/summarize_claim_evaluation.py \
  NEW_OUTPUT NEW_SUMMARY.json NEW_SUMMARY.md
```

Output directories and summary files must be new. The runner records input
annotation, source, and manifest hashes, commands, exit codes, elapsed time,
and peak process RSS. A temporary executable snapshot keeps every pair on
the same binary even if the build directory changes. The runner does not
impose a wall-clock timeout or turn an unavailable measurement into zero.
Raw evaluation outputs, generated JSON summaries, and execution logs are kept
locally and excluded from version control; the commands above regenerate them.

The separate untouched holdout uses
[`claims-holdout.tsv`](../../claims-holdout.tsv). Run it with the same runner
and `--manifest benchmark/realworld/claims-holdout.tsv`. It has no reviewed
change annotations, so its precision and recall are unavailable. It is not
part of the original 39-expectation denominator.

Supplied-role experiments and their source premises are recorded separately
in [`structure-claim-probe`](../structure-claim-probe/README.md). Those
experiments are not production recovery measurements.

## Untouched holdout

The separate IETF Structured Fields pair compares RFC 8941 with RFC 9651.
The source checksums pass, but extraction and comparison are incomplete:
142 unresolved regions, no accepted events, nine candidates, and no review
claims. All 52,711 old and 60,675 new source tokens remain unresolved.
Coverage, precision, and recall are unavailable. The comparison reports
557 ms and 140,623,872 bytes of peak memory in the compact evaluation; the
separate process record includes command startup and report writing.

The manifest's advance expectation of complete extraction was not changed
after observing this failure. No implementation tuning used this pair.
