# Exact native baseline-band indexing

The read-only domain-budget trace identifies the previous controller/processor loss: proposal 288 starts with 4,310 work units, exhausts them during closure, and proposal 289 is never evaluated. The diagnostic comparison and coverage exactly match `joined-stroke-v1`; diagnostic source and release binary were restored before implementation. `domain-budget-diagnosis.json` retains the final trace and frozen log hash.

Bounded evidence validation now retains one extra reference per glyph in a page-local index sorted by finite baseline height and glyph ID. Original page spans and render order remain unchanged. Exact closure queries use inclusive binary-search endpoints, charge lookup work, and inspect every glyph in that band. Unmapped, unassigned and duplicate-height glyphs are retained. The separate paint-order convention accepts roundoff at row boundaries and continues to scan the complete page.

The source, paint, conflicting-view and ownership predicates are unchanged. This removes repeated scans of provably out-of-band baselines, not source evidence or unknown drawing. Index memory is linear in the existing bounded glyph count; construction performs bounded per-page sorting. No resource limit or local proof budget is increased.

A regression fixture contains 10,000 unassigned glyphs on the same page but outside the compared band. The equal interior is certified with 20,000 local work units. Moving an unassigned glyph onto either boundary or into the interior rejects the interval. Existing tests for noncontiguous page storage, reversal and unsafe native evidence remain. Two existing budget tests now recover their valid later change at 2,000 visits instead of requiring 4,000; their forbidden-region assertions remain intact.

## Natural pilot and verification

The six-pair pilot uses the same inputs, arguments and budgets as `joined-stroke-v1`. Compared source references increase by 31 per side for ECB annual, 181 per side for controller/processor, and 329 per side for restrictions. Schedule C, Schedule SE and NIST contingency counts are unchanged. All six remain incomplete; this is not a final 36-pair evaluation or a newly complete pair.

The two lost controller/processor certificates reappear unchanged. Independent raw-source audits verify 70 controller/processor domains and 42 restrictions domains. The additional ECB interval reads `2.4 Notes on the balance sheet ` versus `2.4 Notes to the balance sheet `; its literal all-optimal edit mask is independently verified with edit cost two. These audits do not independently certify boundary correspondence, paint closure, complete inventory or fixed-target recovery.

Final workspace tests pass: 2,514 passed, zero failed, two ignored. Generated verification passes 48/48. Workspace Clippy with warnings denied and formatting checks pass. `baseline-index-checks.json` registers frozen logs; `baseline-index-v1-pilot.json` registers the binary, source archive, inputs and raw captures. The initial targeted run exposed two obsolete budget expectations; both were strengthened to require the valid change at the lower budget before the final passing workspace run.
