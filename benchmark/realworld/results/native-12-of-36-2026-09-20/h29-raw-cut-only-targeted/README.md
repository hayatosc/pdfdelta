# H29 raw-cut-only fallback repair (full016)

The equal-fragment fallback pass now retries only candidates whose first pass
held on `FragmentHold::RawCut`. Eligible candidates are compacted in place into
the already-consumed prefix of `equal_fragment_candidates` (write index at or
before read index, no allocation, no charge change), and only those slots are
revisited with the internal deleted soft line break allowance. Candidates held
on any other check (normalization boundary, sharing, projection, canonical or
raw source, unmapped blocks) are never retried, so they can neither spend
fallback budget nor be reordered.

## Measured outcome

- full016: 36 pairs captured, 0 failed, 3 complete; binary
  `25bf6930760cbde61d29d1bea7d4bd4badf7c8792e6a980dad45bae08645901a`,
  production patch `dcbb2e8221e59dd382072eb67614046d8c3d5616532337386843f6ae49485862`.
- 35 of 36 reports are logically identical to the pre-fallback full014
  baseline; only IRS 1099 differs.
- MEXT lower-secondary review claim is restored: 22 review units with the same
  ordered sequence as full014 (`89188c816fe9`), unresolved 8037.
- IRS 1099 keeps the internal-deleted-break gain: 9 review units identical to
  full015 (`0fd8149c8d7b`), 117 unresolved regions, resolved coverage
  0.8487, work_used 30,323,601 of 32,000,000.
- All 27 review units that full014 emitted for 1099 but full016 does not are
  proven redundant: an instrumented `review::collect` run shows 9 emitted and
  421 skip-resolved, with all 27 in the skip-resolved set, no unvisited
  relations and no budget stop.
- Retention audits (new vs full015) for MEXT, 1099 and schedule-se report
  loss 0 with empty review and problem lists.

## Evidence

- `full016-manifest.json`: 36 rows with 72 input hashes, report logical and
  gzip hashes, CLI exit codes, pin status, and audit bindings.
- `gates.json`: the seven gate commands, return codes and compressed log
  hashes (`gates/h29-repair-g1..g7.log.gz`).
- `1099-review-trace.txt.gz` and `1099-review-hook-patch.diff.gz`: the
  temporary hook used for the redundancy proof, archived before the exact
  restore of production sources.
