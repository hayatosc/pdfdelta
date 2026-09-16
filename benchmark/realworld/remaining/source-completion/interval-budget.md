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
