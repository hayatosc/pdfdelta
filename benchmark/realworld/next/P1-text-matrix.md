# Text retrieval size matrix

`text-matrix.json` registers 16 synthetic cases before measurement: 32, 128,
512 and 2,048 paragraph nodes on each side, with near-equal, fully rewritten,
repeated and mixed-key text. Near-equal text includes a four-digit paragraph
number and changes a trailing value. Rewrite text uses six lowercase versus
uppercase letters, with no common trigram. Repetition uses the same sentence
on each side with a changed final letter. Mixed-key text uses the near-equal
recipe and gives every second node a distinct source key; the existing text
eligibility rule excludes those nodes.

The test-only entry point calls the existing feature builder, indexed enumerator
or frozen dense reference, and production correspondence solver. It does not
implement another matcher. A streaming sequence of serialized proposals hashes
membership, order, basis, supplier and weight. Completed candidate universes must
have equal hashes and counts across modes and repetitions. Incomplete enumeration
withdraws the whole supplemental suffix, as in production. Existing small
exhaustive-oracle tests cover optimal and mandatory assignments independently.

This experiment isolates supplemental singleton retrieval. It excludes base
identity/literal proposals, source-protection solves, group enumeration, PDF
parsing and extraction. Mixed-key results measure exclusion from this supplier,
not recovery of keyed pairs. All constructed nodes have distinct structured
sources, exact scalar views, and source-structure containment. They are not
real-document annotations or evidence of additional strict recovery.

Every case uses the same explicit experimental limits: 64 million token visits,
4,194,304 pair checks and proposals, and the remaining production defaults,
including 200,000 feature entries and one million ownership visits. These are
not new production defaults. Solver limit errors are recorded as incomplete
optimization, not crashes or empty successful comparisons. Retrieval completion
and combined retrieval/optimization completion have separate denominators.

`retrieval_seconds` includes synthetic graph and feature construction plus
enumeration. `optimization_seconds` measures the following solver call. Kernel
time additionally includes candidate hashing and accounting; process time
includes test startup and output. None is PDF end-to-end time. GNU time peak RSS
is maximum resident memory for an isolated process, not allocator live memory.
Production pricing is absent (zero rounds); root/exclusion solve counts are not
exposed, so optimization-run counts remain null instead of fabricated values.
Assignment work is null when the solver returns an error before returning its
accounting record. These observation limits also apply to failed cases.

Reproduce with three alternating dense/indexed repetitions per case:

```sh
PYTHON_UV=0 python benchmark/realworld/next/measure-text.py /tmp/text-matrix-replay
```

The runner builds the release core test executable and retains hashes, settings,
individual results, process costs and summaries. Large raw logs and binaries
stay outside the repository. Fixed PDF controls and their timing regressions
remain in `p1-results/README.md`; this synthetic matrix cannot replace them.

## Recorded outcome

`p1-results/text-matrix/` retains all 96 measurements, executable/source hashes
and summaries. Retrieval completes in 36/48 dense runs and 42/48 indexed runs;
combined retrieval/optimization completes in 24/48 for both. Both modes complete
retrieval for all 32, 128 and 512 cases, with equal proposal hashes and counts.
At 2,048, indexed retrieval additionally completes rewrite and mixed-key cases;
the dense reference exhausts token work. All 2,048 comparisons remain incomplete.
512-node unkeyed cases hit the one-million descendant-ownership limit, while
the 512 mixed-key case returns incomplete optimization without a fatal error.

| 512-node pattern | Dense token visits | Indexed token visits | Dense retrieval seconds | Indexed retrieval seconds |
| --- | ---: | ---: | ---: | ---: |
| Near-equal | 22,613,504 | 4,415,077 | 0.198387 | 0.060010 |
| Rewrite | 4,661,760 | 292,570 | 0.068242 | 0.054011 |
| Repeated | 18,673,664 | 4,570,112 | 0.156532 | 0.058027 |
| Mixed keys | 5,671,424 | 1,144,904 | 0.051167 | 0.017537 |

Times are three-run medians from the recorded host, without CPU isolation;
workspace checks ran during part of the capture. They are observations, not a
controlled speedup estimate. Deterministic work and completion differences do
not depend on that timing limitation. The two 2,048 cases where both modes stop
have worse indexed costs: near-equal retrieval takes 0.912053 versus 0.593292
seconds and peaks at 716,272 versus 148,084 KiB process RSS. Repeated text takes
0.790152 versus 0.562469 seconds and peaks at 660,596 versus 174,064 KiB. Indexing
reaches and allocates more candidates before exhausting the same token budget.
The largest recorded process RSS is 1,002,228 KiB for indexed 2,048-node rewrite.

Retain indexed bounded retrieval, with the existing whole-suffix withdrawal and
production limits. The experiment supports reduced feature work and two extra
completed retrieval cells, not better whole-comparison completion. It provides
no reason to raise production proposal limits. P2 remains a separate measured
deferral; neither experiment resolves source-acquisition or correspondence
limitations. Before capture, the harness incorrectly expected every bounded
solver call to return success; failed exploratory logs remain under
`/tmp/pdfdelta-text-matrix-d9cdc8b`. The final harness records resource errors and
all 96 final processes exit successfully. No production behavior changed here.

Validation: workspace formatting and clippy pass; workspace tests report 2,325
passed and two explicitly ignored measurement entry points. The release runner
executes the new ignored entry point for each registered case.
