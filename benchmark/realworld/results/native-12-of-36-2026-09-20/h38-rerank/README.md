# H38 re-ranking and W4 budget ladder (diagnostic only)

Scorecard: full017 36-pair compact scorecard (`h38-scorecard36.json.gz`, pair
set asserted equal to capture and frozen panel; extraction/work fields unknown
where the capture rows do not carry them). Top incomplete by unowned:
schedule-c 301, faa-maintenance 972, 1099 1841, w2 9389, w4 22454.

W4 budget ladder (`PDFDELTA_DIAG_MAX_WORK` override of max_assessment_work
only; patch `h38-diag-work-override.patch.gz`):
- 32M control: logical equals full017 `ce8cb04b...`; old115/new205 both
  (0,595) unresolved; old 9044/new 9633 resolved; candidates 57, proven 3.
- 64M: old115/new205 unresolved; old 9573/new 10162; retention 32->64 pass.
- 128M: old115/new205 both (0,595) **equal**; old 10387/new 10976;
  retention 32->128 **fail: 3 prior proven payloads lost** (unexplained,
  acceptance not possible).
All three levels use their full budget, so a plateau is not reached
(plateau unknown). Phase profile at 128M: after_stationary 67,336,829 ->
after_raw 46,828,532, i.e. the raw pass consumes 20,508,297, more than the
default pre-local budget of 10,740,479, so reordering alone is not supported.
At 32M the later phases are called with remaining 0 and return early (not
"never called"). The 128M retention failure (3 prior proven payloads lost)
remains unexplained and acceptance is not possible.

Source binding for the 128M resolution (corrected): the earlier zero was a
filter bug (case-sensitive comparison against lowercase "established"). With
the corrected filter the positive control passes: outcome kinds include
tentative and established with established > 0. Target old115/new205 each
expose 594 glyph sources; hits on the same-side target blocks and their exact
ordered source comparison (including synthetic kinds) are recorded in
h38-src.json.gz. The premise is the established RawSourceEquality records 1209/1210
(blocks [112..116]/[202..206], 598 ordered source items = 594 glyph + 4
line_break, exact match with the target). The static call path already
identifies the caller sequence (recover_local -> recover_closed_domains ->
recover_stationary_members -> recover_raw_source_equalities ->
recover_positioned_replacements), so no runtime hook is needed merely to find
the premise. No total completion is claimed. Relations 116/117/507 remain Tentative with reasons
unknown_reading_order, normalization_uncertainty, domain_not_closed at all
levels. The earlier "deferred-tail starvation is the blocker" claim is retracted.
This 595 proof is unavailable at 32/64M and valid at 128M; the full-pair
completion cause remains unproven (all levels exhaust their budget). The
untraced ladder capture directories were rotated by housekeeping; the control
equality (32M) was asserted while both existed, and the 128M phase capture
logical equals the stored untraced full value
447a30b03fdab626cb0d07c318640f22731112ef383d91ce158ea65efde273d2 (see
h38p-binding.json.gz and h38-verify.*.gz).

Phase table:
| phase | 32M remaining | 128M remaining |
| --- | --- | --- |
| before_local | 10,740,479 | 106,740,479 |
| after_local | 0 | 91,046,998 |
| after_closed | 0 | 90,929,374 |
| after_stationary | 0 | 67,336,829 |
| after_raw | 0 | 46,828,532 |
| after_positioned | 0 | 23,980,882 |

Quality gates: the restored source is byte-identical to the H32 final source
(`git diff 884bee1 HEAD -- crates Cargo.toml Cargo.lock` empty, working tree
diff 0), so the five gates and 48-fixture verify recorded in
h32-suffix-full-iteration-017-native/gates.txt are reused.

Retractions: the earlier "no overlapping relation" filter and the
new_span-by-old-block-id join were invalid and are retracted.
