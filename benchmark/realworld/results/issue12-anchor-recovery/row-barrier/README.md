# Row-order proof bounded by unsupported lines

A rotated URL on the SP 800-57 Revision 5 glossary page vetoed every row correction in its leaf region. Bounding-box jitter therefore placed a definition's first line before its term label, splitting the definition into separate blocks. The source render intervals already proved the label-before-definition order.

The row proof now treats unsupported lines as fixed barriers and applies its existing geometry and render-order checks separately to contiguous supported runs. It never moves a supported line across a barrier. `layout-before.txt` and `layout-after.txt` show Association changing from three blocks to a label block followed by one complete definition block. The source-reviewed annotation correction is recorded in `../sp80057-annotation-review/`.

`source.json` and `code.patch` freeze the implementation, executable hash, and checks. Formatting, workspace Clippy with warnings denied, and all 1,968 workspace tests passed. A focused review found no specific issue with the barrier or row-proof contract.

## Comparison

All 29 corpus pairs were evaluated. For the 28 pairs whose annotations did not change, available token metrics, event metrics, quality, reviewed recall, and expected failure lists match the preceding `run-order` captures. Missing metrics remain unavailable. Only IRS Form 1040 changed coverage: old/new coverage fell from 0.516980/0.490659 to 0.506352/0.486039; its reviewed quality and failure list are unchanged.

With the corrected SP 800-57 annotation, the baseline cannot resolve the new glossary start anchor. The retained engine can resolve both scopes and measures token precision 1.0, recall 3/7, and zero false-positive tokens. Its coverage is 0.678232/0.665509, compared with the preceding implementation's 0.669181/0.644684 and the investigation baseline's 0.562817/0.549473. The unavailable corrected baseline quality prevents a precision non-regression claim for this pair.

SP 800-57 still has three expected failures: footer occurrence count 150 versus 157; the Approved comma deletion is not matched as an expected replacement; and Association punctuation remains `reading_order_unresolved`. The earlier engine reported 152 footer occurrences under the diagnostic-only original annotation, so the count change is retained as a limitation rather than hidden by the unchanged aggregate recall.

`association-residual.json` records the next recovery target: old and new complete definition blocks retain gaps 39..45 containing `. For ` and `; for `, and an identical gap 70..91. Exact block-local anchors already recover the surrounding text. The row-order repair fixes source structure; it does not yet establish the missing replacement correspondence.
