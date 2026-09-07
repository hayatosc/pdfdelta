# Token quality across reviewed-scope boundaries

These captures use the original SP 800-57 annotation, preserved in `../sp80057-annotation-review/original-expected.json`. Rendered-page review subsequently found an erroneous glossary replacement and scope anchors in that annotation. The comparisons below remain reproducible historical measurements, but do not establish accuracy against the printed glossary; the corpus now uses the corrected annotation.

Whole-event scope classification rejected SP 800-57 deletion 121 because its old block 293 range 0..48 crosses the glossary scope's start at scalar 39. That rejection previously prevented token-quality measurement as well. The source coordinates themselves were valid.

Token quality now projects every reported change to exact source-token intervals and intersects those intervals with the reviewed scopes. Tokens outside those scopes do not affect scoped precision or false-positive counts. Invalid projections, missing occurrences, spanless occurrences, and resource exhaustion still make token metrics unavailable. Event classification retains its original strict boundary checks; its failure reason is preserved.

## Controlled comparison

Both engine snapshots were evaluated with the same measurement implementation. `comparison.json` records their source manifests, measurement source hashes, binary hashes, and capture hashes. `measurement.patch` contains the benchmark-only change. The baseline checkout matched every file in the original baseline source manifest before the two measurement files were replaced.

The final captures use separate Cargo target directories for the two snapshots. They include the reviewed projection fix: a span with an empty comparable range and a nonempty canonical range, or the reverse, makes token metrics unavailable. All seven captures preserve the earlier measured metrics and coverage after this fix.

SP 800-57 produced identical token metrics before and after the reading-order changes:

| Metric | Baseline | Retained implementation |
| --- | ---: | ---: |
| Expected changed tokens | 38 | 38 |
| Reported changed tokens inside scopes | 29 | 29 |
| True-positive tokens | 25 | 25 |
| Precision | 0.8620689655172413 | 0.8620689655172413 |
| Recall | 0.6578947368421053 | 0.6578947368421053 |
| False positives per 10,000 unchanged tokens | 29.282576866764277 | 29.282576866764277 |

The other five pairs with reviewed scopes were also rerun: Attention, WS-Policy Attachment, BIS operational risk, FIPS 186, and MQTT. Their token metrics, event metrics, quality, reviewed recall, and expected-change diagnostics exactly match the saved `run-order` captures.

Formatting, workspace Clippy with warnings denied, and all 1,966 workspace tests passed. The tests include scope-crossing events, out-of-scope edits, invalid projections, missing occurrences, and occurrence-budget exhaustion.

## Limits

SP 800-57 event quality and expected-change diagnostics remain unavailable because whole-event scope assignment still fails. These missing values are not treated as zero failures. CSF's two expected replacements remain unlinked deletion/insertion output. This measurement repair does not assert that those correspondences were recovered or that the overall issue is complete.
