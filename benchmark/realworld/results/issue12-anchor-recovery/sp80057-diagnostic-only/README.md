# Diagnostic experiment without scope restrictions

This capture uses the retained engine and a temporary copy of the SP 800-57 annotations with the scopes and change scope IDs removed. It investigates downstream failures that the official whole-event scope rejection currently hides. It is not a replacement for the official scoped quality measurement.

The run reports three failures: 152 footer occurrences against the expected 157; a new-side unit segmentation failure for the Approved definition; and unresolved reading order on both sides of the Association punctuation change. The last result identifies the next source-evidence investigation: `purpose. For example,` versus `purpose; for example,`.

The other five annotations were matched by the unconstrained matcher. Those matches do not establish scoped correspondence: substring matches can accept a larger event that crosses a scope boundary, and globally unique quote text alone does not prove event containment. No scoped recall or event precision is inferred from this experiment.

`evidence.json` records the original annotation and executable hashes, the engine source manifest, the experimental input hashes, and the compact failure list. `result.json` retains the full diagnostic output, including recovery-watch evidence. The experiment itself did not change production annotations or the evaluator. A subsequent rendered-page review corrected the corpus annotation; see `../sp80057-annotation-review/`.
