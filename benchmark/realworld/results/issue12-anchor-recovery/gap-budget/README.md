# Preserve exact anchors when optional gap recovery exhausts its budget

The anchored-gap review found that a failed optional gap addition propagated `None` to the entire exact-anchor batch. With 17 tokens on each side and an eight-token minimum anchor, two eight-token anchor pairs consume all four output ranges. The intervening one-token gap cannot allocate two more ranges, and the previous implementation discarded both exact anchors.

The batch now retains its exact anchors and any already committed gaps when optional gap construction stops. Gap additions reserve storage before committing ownership, so an unsuccessful addition does not leave a partial source claim. The regression test checks both retained anchors, the absent gap replacement, and the unchanged output counters.

Formatting, workspace Clippy with warnings denied, all 1,987 workspace tests, and 48/48 generated acceptance cases passed. The generated suite includes all five mandatory core cases. The bounded review reported no other concrete finding; no additional review pass is implied.

All 29 manifest pairs were captured with the fixed executable. Status, coverage, quality availability, scoped token/event metrics, reviewed recall, and expected-change diagnostics match the preceding `anchored-gap` snapshot exactly. `comparison.json` retains a row for every pair, including the five resource-limit outcomes. Unavailable metrics remain unavailable.

`source.json` identifies the executable, source files, full patch against the recorded commit, captures, checks, and review resolution. Apply `code.patch` to that commit and use the command template in `run.json` to reproduce. Earlier annotation limitations and remaining CSF expectations are unchanged; see `../acceptance-status.md`.
