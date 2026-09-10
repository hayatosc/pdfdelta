# Independent assignment pricing experiment

This experiment reuses the production lexicographic integer Hungarian solver,
including its exact row/column potentials and zero-cost unmatched dummy columns.
It is compiled only in core tests. Production eligibility, source decisions and
fallbacks are unchanged. A completed local coefficient problem does not certify
that an earlier PDF candidate supplier finished its universe.

## Immutable universe

`pricing-matrix.json` registers the cases before measurement. Version
`independent-coefficients-v1` fixes each axis to node indices `0..size`, with
proposal ID `row * size + column`. Each synthetic node has a distinct source;
there are no shared sources, groups, or alternative partitions. Normalization
is identity on synthetic coefficients. No PDF glyph correspondence is asserted.
The five score coordinates have the production priority order. All costs are
minimized lexicographically, with one negative weight in the specified class:

| Pattern | Present candidates | Class and weight |
| --- | --- | --- |
| near_equal | All pairs | Class 4; diagonal 1,000,001, others 1 |
| rewrite | All pairs, including zero-overlap pairs | Class 4; 1 |
| repeated | All pairs | Class 1; 1 |
| mixed_keys | Both even indices with equal `index / 4`, or both odd | Even: class 0, weight 1; odd: class 4, diagonal 1,000,001, others 1 |

The mixed case includes duplicate keys (indices 0 and 2, then 4 and 6, etc.).
These are coefficient fixtures, not benchmarks of text feature extraction.
Source/normalization premises are synthetic declarations, not production proofs.
The runner records manifest, implementation, lockfile and executable SHA-256.

## Certificates and incomplete searches

The priced solver initially retains present diagonal edges. After each optimum,
it checks every omitted non-forbidden edge with exact reduced cost
`cost - row_potential - column_potential`. Negative edges are inserted and the
problem is solved again. A full pass with no negative omitted edge proves the
root optimum over this fixed universe. Nonnegative reduced cost is the bound
justifying safe omission; zero reduced cost is not evidence of necessity.

Every selected edge is separately forbidden and priced again from the seed.
Only a strictly worse certified optimum proves necessity. All forbidden-edge
passes must complete before publishing mandatory edges. Exhausted budget,
arithmetic failure, and a failed solve cannot yield that certificate. A root
optimum may be present while the mandatory-edge result remains incomplete.
Each pass records its forbidden proposal, completion, checks, solves and number
of safely omitted edges in its final completed pricing pass. Earlier partial
passes never contribute safe-omission counts. The budget is shared across
construction, all pricing scans and all solves, including forbidden problems.

The exhaustive small oracle covers absent/zero edges, rectangular dimensions,
priority classes, ties and large integer weights. The explicit two-by-two tie
regression shows that omitted zero-reduced-cost edges preserve the root optimum
but destroy the restricted graph's apparent mandatory edges after repricing.

## Measurement

Run from the repository root:

```sh
PYTHON_UV=0 python benchmark/realworld/next/measure-pricing.py /tmp/pdfdelta-pricing-measurement
```

The driver builds release tests once, then runs each case and mode in separate
processes, three repetitions with alternating order. Dense and priced modes use
the same immutable callback, dimensions, objective, and 32,000,000 work budget.
Dense construction queries the entire grid once and retains all present edges;
pricing queries it lazily and reconstructs its active map for each forbidden
problem. Both use the existing optimizer for every solve.

Kernel time includes universe construction, pricing, root optimization and all
mandatory-edge checks. Process time additionally includes test startup and JSON
serialization. GNU time maximum RSS measures live resident process memory in
KiB; it is not allocator live heap. Maximum retained edge count is exact and
reported separately. Candidate checks count visited grid coordinates, including
already-active coordinates; universe lookups count actual callback calls.
Optimization runs include attempts interrupted by the budget. Pricing rounds
include scans interrupted by the budget. Work counts successfully charged work.
All budget failures remain in denominators; an incomplete solve is not a fast
successful certificate. Complete objective/mandatory results must agree.

This kernel experiment alone cannot demonstrate PDF end-to-end improvement or
additional source recovery. Real-document evaluation and the independent text
retrieval size matrix remain separate requirements.
