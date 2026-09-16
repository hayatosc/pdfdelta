# Stage attribution of the fixed baseline

`pdfbench diagnose-report --report <report.json>` reads existing native schema-11
or shared schema-2 output. It does not execute extraction, retrieval, optimization
or comparison, and cannot change operations, masks, coverage or recall. The
diagnostic schema is version 1 and binds findings to the original report SHA-256.

Eight stages are distinguished: `acquisition`, `normalization`, `scope`,
`retrieval`, `optimization`, `counterpart_decision`, `localization`, and
`reporting_evaluation`. Attribution uses the owning diagnostic fields, explicit
completion flags and known native reason codes/shared reason messages. It locates
observed failures; it is not a complete causal model. Unknown reason messages
remain `stage: null`, including native `search_incomplete`/`work_limit` messages
that do not identify which search stopped. Positive anchor/source hints in native
unresolved-region evidence are not classified as failures.

Each reason group retains its observation count and up to four JSON pointers
into the original report. If `observations` exceeds the number of pointers, the
remaining locations are summarized, not enumerated. Different reasons and stages
may describe the same dependency. Counts must not be added to estimate failed
events, missing changes, or independent failed scopes. Missing findings do not
mean successful or complete stages. In particular, normalization failures that
prevented candidate creation but were not serialized cannot be reconstructed
from the final report. Acquisition findings include retained issues in unselected
channels; they do not independently determine selected-channel completeness.

The input limit is 128 MiB; shared reports additionally retain the existing
64 MiB parser limit. Native text/source payloads are skipped during deserialization.
Failed reads, invalid schemas and oversized reports emit a
`reporting_evaluation` finding and exit 2. Successful diagnosis exits 0 even if
the PDF comparison is incomplete. A diagnostic read failure does not imply an
acquisition failure or an empty comparison result.

## Captured observations

These nine captures reuse the original baseline reports and verify their hashes
against `../baseline-3f318d7/runs.json`; PDF comparisons were not rerun.
`summary.json` retains each report hash, diagnostic exit code and observation
counts. Source inputs and comparison settings remain those in the baseline.

- IRS 1040 native: 148 normalization and 461 scope observations; these are not
  counts of distinct text changes or distinct failed blocks.
- EDPB shared text: 5 retrieval and 1 optimization observations, with 178
  counterpart-decision observations. This distinguishes incomplete search from
  insufficient correspondence evidence without assigning either sole causality.
- EDPB native: the 1.9 GB report exceeds the diagnostic limit. Its failed read
  remains in the nine-run denominator and its verified report hash is retained in
  the summary, even though the unread diagnostic itself has no report hash.

Recreate the original reports using the baseline capture command in
`../baseline-3f318d7/README.md`, then diagnose each report:

```sh
cargo run -p pdfdelta-bench --locked -- diagnose-report \
  --report /tmp/pdfdelta-baseline-replay/raw/irs-form-1040-2024-to-2025-text.json
```

Timing fields can change raw report hashes on replay; the diagnostic always binds
to the report actually read. Unit tests cover known/unknown native reasons,
positive-evidence exclusion, bounded location examples, malformed reports,
shared source pointers, hash binding and unchanged operational summaries.
