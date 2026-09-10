# P2 independent coefficient pricing measurements

Decision: retain P1 and the existing assignment fallback as the production
default. The experimental pricing implementation is test-only. It reduces live
edge retention, but does not improve completed certification under the fixed
32,000,000 work budget: at 128 elements dense completes all four patterns in all
three repetitions, while pricing completes none. At 32 elements both complete,
but pricing has higher median kernel time in all four patterns. This is a measured
rejection of this implementation, not a claim that all exact-pricing designs fail.

## Results

Each cell is the median kernel time in milliseconds across three isolated release
processes. Completion counts include the root and every forbidden-edge problem;
the denominator is three runs per mode per case. Incomplete runs remain timed,
but their smaller time is not a speedup for a completed result.

| Case | Dense ms | Priced ms | Dense complete | Priced complete |
| --- | ---: | ---: | ---: | ---: |
| near-equal-32 | 5.60 | 6.51 | 3/3 | 3/3 |
| rewrite-32 | 2.50 | 8.31 | 3/3 | 3/3 |
| repeated-32 | 2.80 | 7.29 | 3/3 | 3/3 |
| mixed-keys-32 | 2.16 | 4.61 | 3/3 | 3/3 |
| near-equal-128 | 330.24 | 285.08 | 3/3 | 0/3 |
| rewrite-128 | 121.25 | 230.54 | 3/3 | 0/3 |
| repeated-128 | 127.80 | 237.33 | 3/3 | 0/3 |
| mixed-keys-128 | 117.20 | 179.38 | 3/3 | 0/3 |
| near-equal-512 | 519.77 | 257.16 | 0/3 | 0/3 |
| rewrite-512 | 475.70 | 235.43 | 0/3 | 0/3 |
| repeated-512 | 486.72 | 245.29 | 0/3 | 0/3 |
| mixed-keys-512 | 270.70 | 175.86 | 0/3 | 0/3 |
| near-equal-2048 | 1604.58 | 200.99 | 0/3 | 0/3 |
| rewrite-2048 | 1539.42 | 193.23 | 0/3 | 0/3 |
| repeated-2048 | 1590.33 | 204.66 | 0/3 | 0/3 |
| mixed-keys-2048 | 528.33 | 165.54 | 0/3 | 0/3 |

There are 96 attempts, all producing measurement records. Dense completes 24/48
and pricing 12/48. The synthetic case mix is not a PDF population estimate.
Completed certificates agree across repetitions and modes. The small exhaustive
oracle additionally checks both implementations against all optimal partial
matchings; its purpose is correctness, independent of these timing observations.

At 128 elements, maximum retained edges fall from 16,384 to 254 (near-equal),
381 (rewrite/repeated), and from 4,224 to 190 (mixed keys). However, pricing visits
3.13–4.17 million candidate coordinates versus dense's 16,384 and attempts
191–255 optimizations versus dense's 129 before its budget expires. The existing
Hungarian solver still scans the full coordinate domain during augmentation;
sparsifying its stored edges does not sparsify those scans. Rebuilding/pricing
each forbidden problem adds further work. The omitted-tie regression requires
this renewed proof; skipping it would incorrectly declare mandatory edges.

At 2,048 elements maximum RSS is about 821,000 KiB dense versus 8,700 KiB priced
for the all-pairs cases. Both modes are incomplete. This is peak resident process
memory, not allocator live heap or proof of usable large-case completion.
Full counters, timing samples and process memory measurements are retained below.

## Artifacts and reproduction

- `../pricing-matrix.json`: immutable coefficient universe and fixed work budget.
- `../P2-pricing-contract.md`: priorities, node/source/normalization conventions,
  reduced-cost proof, forbidden-edge handling and measurement definitions.
- `metadata.json`: source/manifest/lockfile and executable hashes, Rust and OS.
- `observations.json`: all 96 process results and kernel timing observations.
- `summary.json`: candidate checks, actual universe lookups, pricing rounds,
  optimization attempts, maximum retained edges, time, memory and completion.
- `certificates.json`: one deterministic trial per case/mode, with all forbidden
  IDs, completion flags and final safe-omission counts. `pass_columns` names each
  compact pass array's fields. Repetitions have identical non-timing trial data.

Certificate hashes in observations are SHA-256 of the original trial object,
excluding `kernel_seconds`, serialized with Python `json.dumps(sort_keys=True,
separators=(',', ':'))`. To reconstruct that object from a compact certificate,
remove `certificate` and `sha256`, and replace each pass array with
`dict(zip(pass_columns, pass_array))`. This preserves all original trial data.
The pre-commit HEAD in metadata identifies the base; source hashes bind the
uncommitted experimental implementation used for these measurements.

```sh
PYTHON_UV=0 python benchmark/realworld/next/measure-pricing.py /tmp/pdfdelta-pricing-rerun
```

The runner emits uncompressed trial records and raw per-process logs outside the
repository. An earlier pilot was repeated after adding the dense-oracle check;
only the final source-hash-bound run is summarized here. No input PDF, build output
or raw process log is included in these permanent results.

The experiment measures construction through full assignment certification, not
PDF parsing, feature extraction, source decisions or reporting. It supplies no
new A/B source recovery and is not a substitute for the text retrieval size
matrix, registered development/blind documents, or PDF end-to-end evaluation.
Future production adoption requires a measured completion/cost improvement,
source/normalization-bound universe integration and the existing independent
assignment eligibility checks. Shared sources, groups and alternative partitions
must retain their separate solver paths.
