# Reproduce lost interval coverage

The tagged-region fixed panel loses one earlier ECB interval, containing 31 native
references on each side. Its reading changes `2.4 Notes on the balance sheet ` to
`2.4 Notes to the balance sheet `. The preceding inline-projection executable
reproduces the same loss; it was not introduced by tagged-region support.

`interval-budget-trace-v1-pilot.json` binds a diagnostic executable, source archive,
inputs and raw report. Temporary instrumentation records preparation work in
stderr. Comparison and coverage are identical to the uninstrumented tagged-region
capture. The instrumentation was removed after capture.

The lost interval's report-local boundaries are 7727/7728. Preparation begins
with 41,203 work units and fails with five remaining. Subsequent candidates exhaust
the remainder. Ten preceding intervals reach preparation; preparation repeatedly
rebuilds an immutable graph-node lookup for each side. Reuse of this lookup is a
bounded implementation opportunity; increasing the budget or accepting an
unfinished proof is not a remedy. This reproduction establishes no new complete
pair and does not attribute every preparation cost to index construction.

## Reuse immutable lookups under the same budget

Whole-node interval preparation now builds each side's graph-node lookup lazily
once per append call. Construction and each lookup consume the existing shared
proof budget. Projection, full native population, boundary premises, ownership
and exact comparison are still revalidated for every candidate. Nothing is cached
across mutable views or documents, and no work allowance is increased.

The five-pair pilot in `interval-index-v1-pilot.json` restores the lost 31/31
references and adds two more ECB intervals: another `on` to `to` heading change
(41/41 references), and removal of a raised `1` after `report` (79/78). Compared
references rise from 45,927/45,912 to 46,078/46,062, a gain of 151/150. All preceding
intervals and the rest of the ECB comparison object are retained. SSDF, Schedule
C, Schedule SE and EDPB controller/processor comparison and coverage are unchanged.
All five remain incomplete. This is not a new full-panel or final two-run result.

`interval-index-v1-audit.json` binds fresh same-binary native worker captures and
an independent all-optimal-edit-path oracle. The three literal readings, unique
source ownership, boundary exclusion and exact masks pass with edit costs 2, 2
and 1. The oracle passes 225 exhaustive short controls. This does not independently
certify acquisition, composited visibility or boundary correspondence, and is not
fixed-body-target recovery.

A bounded fixture with three changed regions and 1,000 unrelated container nodes
retains all three exact single-character masks at 10,000 work units, while zero
budget produces no owned intervals. The 94 related tests, 2,518 workspace tests
(two ignored), 48 generated PDF cases, formatting and all-target Clippy pass.
The reproduction precedes the implementation in commit `898b9f5`. Inventory and
search obligations remain independent; there are zero newly complete pairs.
