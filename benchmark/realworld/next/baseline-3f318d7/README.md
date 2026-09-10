# Fixed operational baseline

Implementation and binaries: `3f318d7fd5cb0911e78f47fe05ea4460ae54c633`.
Captured on 2026-09-10 UTC (2026-09-11 JST). All nine measured comparisons return
exit code 3 (incomplete). These are operational measurements, not annotation recall.
The three routes have different result contracts; a difference between routes is
not a performance improvement attributable to a candidate-index change.

| Pair | Route | Seconds | Peak RSS KiB | Report bytes |
| --- | --- | ---: | ---: | ---: |
| IRS 1040 | native | 0.78 | 49,696 | 74,145,342 |
| IRS 1040 | text | 0.44 | 33,552 | 462,848 |
| IRS 1040 | all | 0.57 | 34,208 | 1,728,050 |
| EDPB access | native | 46.28 | 731,484 | 1,913,676,509 |
| EDPB access | text | 24.40 | 446,076 | 3,212,519 |
| EDPB access | all | 23.94 | 446,820 | 3,214,372 |
| IRS W-4 Korean | native | 0.81 | 86,992 | 70,309,695 |
| IRS W-4 Korean | text | 0.72 | 99,660 | 424,026 |
| IRS W-4 Korean | all | 0.81 | 99,100 | 6,640,297 |

The EDPB native report exceeds the adopted 128 MiB summary-input ceiling. Its
comparison is measured and retained, but its compact semantic summary is omitted.
`attempts.json` records the initial unbounded summary and interrupted capture;
completed comparisons were retained when outstanding routes were resumed.
Shared-report parsing additionally enforces its existing 64 MiB limit.

EDPB shared text enumerates 627 candidates, with incomplete enumeration and
optimization and zero conditional or inferred operations. This supplies a concrete
candidate-search control; it does not isolate all end-to-end cost to retrieval.

Build both executables from the immutable implementation commit in a separate
checkout with `cargo build --release --workspace --locked`, then run from this
repository root:

```sh
bash benchmark/realworld/next/capture-baseline.sh \
  /path/to/baseline/pdfdelta /path/to/baseline/pdfbench \
  benchmark/realworld/cache /tmp/pdfdelta-baseline-replay \
  3f318d7fd5cb0911e78f47fe05ea4460ae54c633
```

Use `inputs.tsv` with the existing fetch/verification workflow if cache inputs are
missing. `runs.json` binds measurements to input and raw-report hashes. Raw reports
and binaries remain outside version control. Environment and lockfile/executable
hashes accompany the measurements. Only committed compact artifacts are needed to
select inputs and repeat the capture; absolute paths in executable hash records
identify the original local capture and are not required replay paths.

Validation at this checkpoint: formatting, workspace clippy, workspace tests
(2,293 tests), and generated fixture verification (48/48). actrun could not create
its worktree under the read-only `.git` boundary, so CI commands were run directly.
