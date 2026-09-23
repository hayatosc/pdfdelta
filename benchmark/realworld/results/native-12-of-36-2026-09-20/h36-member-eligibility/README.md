# H36 member eligibility (diagnostic only)

H35 hook re-applied plus one added view-dump field: `block_candidates` (the
Vec<bool> already computed in `build_views`). No new proof or charge work.
Bounded IRS1099 capture: 1786 rows, binary
7ab44b30703bbd8eaa8ebb4f4c7e7073f5fe75e090f2e0b9a24e58d45b7b987f, logical
report 7964e3b5179c00edcc9a229b71bc0e9483c9975cf8204f92e51b06689a5773b2,
no truncation and no diagnostic I/O errors. Core hooks restored byte-exact.

## Measured split of the 525 view-rejected tokens

Joined per round-1 side-0 view (block_indices and block_candidates asserted to
have equal length) to the H33 census 1044/49:

| kind | singleton | member_candidate | tokens |
| --- | --- | --- | --- |
| TrustedRunId(65) | multi | false | 342 |
| TrustedRunId(0) | multi | false | 4 |
| TrustedRunId(41) | multi | true | 34 |
| TrustedRunId(18) | singleton | true | 28 |
| TrustedRunId(37) | multi | true | 25 |
| TrustedRunId(77) | multi | true | 25 |
| TrustedRunId(61) | multi | true | 23 |
| other Trusted runs | multi/singleton | true | 44 |

Totals: member_candidate true 179 (singleton 30, multi 149), false 346.
Member candidate eligibility is therefore not the blocker for 179 tokens; the
342-token run 65 block is the dominant non-candidate multi member. True does
not waive regional closure, uniqueness or source-issue checks.

Files: h36-trace.jsonl.gz, h36-hook.patch.gz, h36-aggregate.py.gz,
h36-aggregate.json.gz, h36-aggregate.log.gz, capture.log.gz, summary.json.gz,
binding.json.gz.
