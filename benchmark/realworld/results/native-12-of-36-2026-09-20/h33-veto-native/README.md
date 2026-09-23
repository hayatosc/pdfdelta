# H33 retry first-veto trace and record correlation (diagnostic only)

Temporary env-gated trace at three real return branches of
`retry_equal_fragment` (contiguous_Held, fallback_Held, adopted). No new proof
or charge calls; 8 MiB truncation cap; diagnostic I/O failures printed. One
bounded IRS1099 native capture; capture logical report SHA256 matches H32
7964e3b5179c00edcc9a229b71bc0e9483c9975cf8204f92e51b06689a5773b2. Hook
restored byte-exact; patch archived here.

## Retraction

The earlier "299 == 300 byte-identical" claim is wrong: the records differ in
`parent`. Relation 300 is a child proposal whose `parent` is the established
domain proof 299, and 784 is a child of 783. The zero trace rows for 299/783
are explained by that parent/child split, not by an early return.

## Actual holds and residual join

The join uses the direct union of each census segment's `eq_ids`, `ch_ids` and
`other_ids`, matched against `trace.row.relation`; parent links are explanatory
only. All 95 trace rows were asserted against their real report records (old
and new blocks, canonical and comparable ranges), the report logical hash was
recomputed from the gzip stream and asserted equal to
7964e3b5179c00edcc9a229b71bc0e9483c9975cf8204f92e51b06689a5773b2, and the
117 segments were asserted disjoint with unowned totals 1044/797.

| census class | hold | relation (child of parent) | covered tokens (old/new) |
| --- | --- | --- | --- |
| established-equal | contiguous_Held PositionMismatch | 300 (parent 299) | 16 / 16 |
| established-equal | contiguous_Held Projection | 784 (parent 783) | 2 / 2 |

Of the 1044/797 unowned comparable tokens the traced branches directly cover
18/18; the remaining 1026/779 are untraced by these three branches and stay
unknown. The Projection hold means the large proposal's proof is held, not that
individual tokens have a source anomaly; no such claim is made. The earlier
parent-inference join and block-length estimate are removed.

Files: retry-veto-trace.jsonl.gz, hook-retry-veto.patch.gz, capture.log.gz,
summary.json.gz, binding.json.gz, correlate.py.gz, correlate.log.gz,
correlate-summary.json.gz.
