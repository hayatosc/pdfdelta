# P1 indexed candidate controls

The text supplier now groups compatible kinds and transfers right-side gram maps
into an inverted index. It accumulates the same multiset intersection counts and
still emits every compatible singleton pair, including zero-overlap weight-one
pairs, in the old order. Equal gram multisets only trigger a check of original
tokens; they do not establish literal equality. Existing source identity/literal
indices, source-only protection, groups, normalization and solver proofs remain
unchanged. `../P1-candidate-contract.md` freezes the pre-change contract.

## Evidence and adoption

Retain the indexed kernel as the default bounded enumerator: 96 small cases match
the frozen dense reference in proposal membership, weights and order, and their
mandatory edges match an independent enumeration of all partial assignments.
Additional tests cover zero overlap, equal multisets with different text,
normalization families, and pair/proposal/token-budget withdrawal without losing
earlier source candidates. Existing solver tests retain their broader priority,
duplicate-key and assignment coverage.

The Korean IRS W-4 control exercises the changed kernel. Both selected routes keep
36 examined compatible pairs and 9,407 feature entries. Charged token work falls
from 147,788 to 81,034 (45.2%); 4,935 right-side posting entries replace the
per-node maps. This reduces bounded feature work, not the candidate universe.
These counters are conservative work accounting, not CPU-cycle measurements.

All 14 paired executions across three fixed pairs and two routes have identical
full result contracts after removing only top-level wall time and the three text
search execution counters (`examined_pairs`, `token_visits`, `index_entries`).
This comparison retains operations, masks, source references, inferred status,
dependencies, coverage and completion. All 28 process executions return 3;
comparison completion and strict recall have not improved in these controls.

| Pair | Route | Repeats per binary | Baseline seconds | Indexed seconds |
| --- | --- | ---: | ---: | ---: |
| IRS W-4 Korean | text | 5 | 0.79 | 0.78 |
| IRS W-4 Korean | all | 5 | 0.96 | 0.97 |
| IRS 1040 | text | 1 | 0.46 | 0.44 |
| IRS 1040 | all | 1 | 0.52 | 0.51 |
| EDPB access | text | 1 | 27.89 | 28.58 |
| EDPB access | all | 1 | 28.68 | 30.44 |

Values are process-time medians; single observations do not establish a timing
trend. The text/all W-4 deltas are small and mixed. EDPB's slower observations
are retained, not explained away as an improvement. In the IRS 1040 and EDPB
controls, incomplete source candidate enumeration prevents the text supplement
from running at all; this indexing change cannot resolve that earlier boundary.
No general PDF end-to-end speedup is established. Maximum process RSS values and
all timings remain in `summary.json` and `runs.json`; RSS is not solver live-heap
telemetry. The full size matrix, P2 experiment and broader development/blind
evaluation remain required before the overall performance goal is complete.

## Reproduction

Use a release executable built from immutable baseline
`3f318d7fd5cb0911e78f47fe05ea4460ae54c633` and one built from this implementation.
The metadata records the candidate parent and the hash of the production prefix
of `text_candidates.rs`; test-only cleanup after compilation does not alter that
prefix. Executable and input hashes accompany each pair. Original paths in the
hash records identify the capture and are not required replay paths.

```sh
bash benchmark/realworld/next/measure-indexing.sh \
  /path/to/baseline/pdfdelta target/release/pdfdelta \
  benchmark/realworld/cache/irs-w4-korean-2024-to-2025-old.pdf \
  benchmark/realworld/cache/irs-w4-korean-2024-to-2025-new.pdf \
  /tmp/pdfdelta-index-replay 5
```

Repeat with the IRS 1040 and EDPB inputs in `../baseline-3f318d7/inputs.tsv` and a
repeat count of 1 to reproduce the remaining controls. Each route uses default
scale 1 and a 180-second process ceiling. Order alternates by repetition. Raw
reports, normalized contract copies, process logs and build outputs stay outside
the repository. No parser, extraction, or annotation behavior changed.

Validation: formatting, workspace clippy, 2,308 workspace tests, and all 48
generated acceptance/regression renderer cells passed.
