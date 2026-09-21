# H16 witnessed impossible cut

Status: implemented and fully verified against the accepted H14 baseline; the
final full-panel capture (`h16-full-iteration-006-native`, binary
`a89ba7eb31c8a1a4fe735271aeab0b485dd78c893f7921bbf1973280bb1a9c6b`) keeps
33/36 whole reports logically byte-identical to H14, gains +222 resolved
tokens per side on `irs-w4-english-2024-to-2025`, and passes deep retention on
all three differing pairs. Score remains 3/36; W4 still has 345 unresolved regions.

## Mechanism

`MandatoryMatchAnalysis::crossing_witness(a, b)` returns a mandatory equal
pair `(p, q)` with `(p <= a && q > b) || (p > a && q <= b)`. Every maximum
matching then passes on opposite sides of the point, so the cut cannot lie on
any optimal script. The search uses one `partition_point` per cut (prefix
maximum `q`, suffix minimum `q`) over the rank-sorted mandatory pairs.

`Assessor::witnessed_impossible_cut` consults only an already-cached analysis,
requires both proposal spans, preflights and charges a bound covering the
parent block/token scan, the child blocks and two conservative binary-search
budgets before localizing, preserves the shared remainder when unaffordable,
propagates real localization errors, and leaves the ordinary proof path
unchanged on a cache miss. It is applied to both
`boundary_displacement_proof` (as `NotProven`) and
`proposal_edits_are_invariant` (as `NotInvariant`), because skipping only the
former would spend the same budget in the latter.

## Oracle tests

- `binary_search_witness_matches_the_linear_scan`: existence matches the scan
  and every returned witness crosses, over all small words.
- `crossing_witness_excludes_the_cut_from_every_optimal_vertex_set`: uses the
  independent matching enumeration plus `matching_hunks`/
  `independent_on_script`; every witnessed cut is absent from every optimal
  vertex set (including origin, end and pure insertion/deletion corners) and
  fails the per-path point predicate.

## Real-path regression

`witnessed_impossible_cut_vetoes_the_invariant_proof_on_the_real_path` uses
genuinely unequal parents (seven "ABC" blocks plus one "ABD" block against
eight "ABC" blocks) so the equal-input fast path cannot run and the 24x24
suffix table is unaffordable at the test budget. The cache is populated
through the real `forced_equal_child` sibling path (asserting a positive
result and cache presence) before shrinking to the discriminating budget.

Both consumers are covered and independently discriminable:

- Invariant consumer (`proposal_edits_are_invariant`): off-diagonal proposal
  with cached witness at budget 200 returns `NotInvariant`; replacing the
  production veto with `if false` fails the assertion
  "the cached crossing witness must veto without enumeration budget"
  (exit 101, `mutation-invariant-consumer.log`).
- Boundary consumer (`boundary_displacement_proof`): empty
  `ExactDisplacementInput` sidecars activate the negative-only path; with the
  cached witness at budget 250 the proposal returns `NotProven`; replacing
  that veto with `if false` fails the boundary assertion (exit 101,
  `mutation-boundary-consumer.log`).

A cached-but-unaffordable helper case (remaining 10) asserts the helper
refuses without spending the remainder, and a cache miss at budget 0 falls
back unchanged. After each mutation the file was restored byte-identically
(verified by SHA-256).

## Final full-panel capture (binary `a89ba7eb31c8`)

Compared against pinned `h14-full-iteration-005-native` (binary
`c2c4d152d261`) with `native_capture_delta.py`: 0 rows require explanation.
Totals: old resolved 90,025 -> 90,247 (+222), new resolved 185,996 -> 186,218
(+222), unresolved regions 76,644 -> 76,615 (-29). Pair-level results and the
full hash bindings are in `full36-manifest.json`; the rendered table is
`full36-delta.md`.

| pair | H14 | H16 | logical report | retention |
| --- | --- | --- | --- | --- |
| irs-w4-english-2024-to-2025 | 5,725 / 6,314, 374 unresolved | **5,947 / 6,536, 345 unresolved** | differs | pass: +222/222, prior loss 0 |
| irs-schedule-c-2024-to-2025 | 6,619 / 6,634, 19 unresolved | identical metrics | differs (work counters only) | pass: no coverage change |
| nist-sha-1803-to-1804 | 5,946 / 5,944, 1,013 unresolved | identical metrics | differs (work counters only) | pass: no coverage change |

The remaining 33 reports are logically byte-identical to H14 and carry
per-pair logical and stored hash bindings in `full36-manifest.json`.

## Verification

All seven required gates exited 0 on this source state: `cargo fmt --check`,
workspace clippy with `-D warnings`, `cargo test --workspace`, all-features
library tests (1,401 passed, 2 pre-existing ignored), rustdoc with
`-D warnings`, generated-fixture `pdfbench verify` (48/48), and
`git diff --check`. The full log with source binding and per-command exit
markers is `gates.txt`.

## Acceptance commit

Committed as `bef9fae1b30fd982e2e80c51d81066209fd732ed` (engine, tooling and this evidence directory; the
H14 manifest hash bindings and the neutral delta title share the commit).

## Candidate binding

| artifact | SHA-256 |
| --- | --- |
| `crates/pdfdelta-core/src/diff/assessment.rs` | `385850188704ef47b83757534252a26c9df1151c99e5aad691f40cacaea6bcdb` |
| `crates/pdfdelta-core/src/diff/assessment/semantic.rs` | `47023ce1c98c9996d31e48362d72ff755114a4d40ea8d749347cc232b21baeba` |
| capture binary `target/release/pdfdelta` | `a89ba7eb31c8a1a4fe735271aeab0b485dd78c893f7921bbf1973280bb1a9c6b` |
| H14 summary / H16 summary | `29aa561c3585…` / `06e629cbb75a…` (full digests in the manifest) |

The temporary `candidate.patch` was removed as superseded by the acceptance
commit; the source hashes above and the capture archives are the durable
bindings.
