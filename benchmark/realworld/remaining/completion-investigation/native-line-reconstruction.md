# Native report reader fix and line reconstruction correction

Two bounded corrections: the native report reader must accept real serde-pretty
success reports (A), and native line reconstruction must not interleave
independent overlapping runs (B). Both were measured on the small Schedule SE
and Schedule C pairs; no other captures were re-run. Strict completion remains
**0/36** and this report does not claim otherwise.

## A. Reader accepted no-change reports

`capture.py` reads native reports with a bounded, structure-aware reader instead
of parsing multi-gigabyte assessments. It assumed every container closes on its
own indented line, but serde pretty prints an empty `Vec`/`Map` as `[]` / `{}`
on one line. A report with all difference arrays empty (a complete no-change
result) had no indented `]` line and was rejected; an empty array followed by a
non-empty one made the reader skip every member up to the next indented
terminator.

The reader now recognises the two-byte empty spellings before searching for a
terminator, and rejects missing commas, trailing commas and repeated members.
The bounded-memory, type, coverage, truncation and trailing-data checks are
unchanged.

Evidence that real output parses:

- Four tiny inputs were run through the real CLI (`--native-text-only`):
  identical text (exit 0, complete, no content change), an exact replacement
  (exit 1, complete, one content change), an image-only pair (exit 0, complete
  and vacuous), and a corrupt Flate stream (exit 3, both extractions
  incomplete). Their reports contain 10–21 empty containers each and all pass
  the production reader and summarizer.
- The four real reports were overlaid into a 36-row fixture capture and the
  production collector verified them with the same panel, runs, binary, input
  and hash binding as the frozen panel: 34 complete non-vacuous, one vacuous,
  one incomplete, no failures.
- The reader change was cross-checked against the frozen Schedule SE report:
  complete `false`, status `detected`, 21 unresolved regions, 4 tentative
  candidates, coverage 0.9174/0.9148 — identical to the frozen v1 row.

The collector's self-test now holds 31 controls, including the empty-container
fixture, missing comma, trailing comma, duplicate member, and the four real CLI
reports. Raw outputs: `reader-controls.json` and `real-reports/results.json` in
the cache.

## B. Line reconstruction interleaved independent runs

### Hypothesis and geometry

`layout/line.rs::candidate_score` admits a glyph outside the baseline tolerance
when its cross-axis interval overlaps the line and its inline gap stays within a
fraction of the font size. The inline gap is measured against the line's union
interval, so an independent string drawn inside that union gets a gap near zero
even when its baseline is far away.

Schedule SE block 4 shows this exactly:

- The 20 pt year field `2024` is painted first (render order 254–257), bbox
  y 721.3–746.6, baseline y 726.72.
- The 7 pt label `Attachment` follows (render order 260–272), bbox y 717.5–725.7,
  baseline y 719.0.
- Baseline distance 7.72 exceeds 0.25 × the line's 25.3 median height, but the
  cross-axis overlap is 0.54 of the shorter interval and the inline gap is 0.7,
  so every label glyph joined the year line. `finish` sorts by x-center, giving
  `Atta2chm0ent 2 4` (and `… 2 5` on the new side).

The mixing is a line-formation defect, not missing acquisition: both glyph runs
are native text with complete extraction.

### Fix

A new relative option `LineOptions.max_script_font_size_ratio` (default 2.0)
applies only when a glyph is outside the baseline tolerance: it may join the
line only when the larger of the glyph and line median font sizes is within
that factor of the smaller. Attached superscripts and subscripts stay within
the factor; independently drawn overlapping runs at very different sizes do
not. No document, publisher or year rules, no flag suppression, no text
dropping, and layout stays reversible.

Red → green: `separates_an_overlapping_independent_run_from_a_larger_baseline`
uses the real Schedule SE geometry and failed before the fix (one line), then
passed (two lines). Existing controls stay green: mixed-size superscript on one
line, staggered columns with attached scripts, and same-baseline mixed sizes.
`max_script_font_size_ratio` also rejects invalid configuration.

The diagnostic `crates/pdfdelta-bench/examples/layout_line_probe.rs` dumps
reconstructed lines with raw glyph baselines, sizes, bboxes, render order and
provenance. On the current build the Schedule SE block 4 lines are `2024` /
`2025` (20 pt) and `Attachment` (7 pt), separately.

### Measured effect (one run per pair, limits scale 1, under the 6G scope)

| Pair | Established changes | Unresolved regions | Tentative candidates | Coverage old/new | Strict complete |
| --- | --- | --- | --- | --- | --- |
| SE old | 8 | 21 | 4 | 0.9174 / 0.9148 | false |
| SE new | 8 (preserved) | 19 | 3 | 0.9176 / 0.9149 | false |
| C old | 7 | 113 | 8 | 0.7405 / 0.7421 | false |
| C new | 7 (preserved) | 116 | 7 | 0.7625 / 0.7640 | false |

- SE: all eight established changes are preserved (block ids shift by one after
  the split); two unresolved regions and one tentative candidate disappear. The
  block 4 year change is no longer a tentative candidate but a two-sided
  unresolved region (`2024 Attach…` / `2025 Attach…`, `reading_order_unknown`).
- C: all seven established changes are preserved; coverage rises by 0.022 on
  both sides (about 148 more resolved tokens per side) and one tentative
  candidate disappears, while unresolved regions rise by three because one-sided
  spans merge into two-sided regions.
- Extraction stays complete on both sides for both pairs; no new strict
  completion is added.

### Where `reading_order_unknown` still comes from

The trace metrics show the remaining evidence is produced by the region
partition, not by line formation: `uncertain_lines_untrusted_in_known_order` is
8 → 9 for SE and 42 → 41 for C. Those lines are outside the trusted runs of a
known region order, so the alignment marks their spans `reading_order_unknown`.
The line fix corrects the interleaving but does not change that classification.
The next native investigation should target the trusted-run/region
classification (why the header, footer and form-field lines are untrusted), the
remaining `candidate_competition` regions (for example `168,600` /
`$176,100`), the footer/header `text_similarity` + `anchor_interval` regions,
and the `exact_canonical` two-sided regions.

## Remaining native obligations

Both measured pairs remain incomplete and the panel-wide status is unchanged at
**0/36**:

- Schedule SE: 19 unresolved regions (mostly `reading_order_unknown` around
  blocks 4–7, 39/40, 1/2, 93/95; `candidate_competition` at 48 and 82;
  `text_similarity` + `anchor_interval` at 73/74; `exact_canonical` at 3),
  3 tentative candidates (footer year, footer `Created`, page 2 year), coverage
  0.9176/0.9149.
- Schedule C: 116 unresolved regions with the same `reading_order_unknown`
  dominance, 7 tentative candidates, coverage 0.7625/0.7640.
- The five extraction-incomplete panel pairs and the panel-wide unresolved
  native regions recorded in the scope-correction report are untouched by this
  work.

## Quality gates

Run serially after the production change:

- `cargo fmt --all -- --check` — pass.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass
  after two small lint fixes (an unused variable in a CLI test helper and
  `cloned`→`copied` in the new diagnostic example).
- `cargo test --workspace` — 2661 passed, 2 ignored.
- `cargo test --workspace --all-features --lib` — 1515 passed, 2 ignored.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
  --document-private-items` — pass.

## Evidence index

- Cache summary and hashes:
  `benchmark/realworld/cache/completion-investigation/native-line-reconstruction-v1/summary.json`.
- Reader controls: `…/reader-controls.json`; real CLI check
  `…/real-reports/{inputs,reports,results.json}`.
- SE/C comparison: `…/se-c/comparison.json`, new reports under `…/se-c/`, old
  reports referenced read-only from
  `benchmark/realworld/cache/completion-investigation/native-text-scope-v1/`.
- Traces and line dumps: `…/traces/`.
- Code: `benchmark/realworld/remaining/completion-investigation/capture.py`,
  `collect_native_text_scope.py`, `native_scope_fixtures.py`;
  `crates/pdfdelta-core/src/layout/line.rs`;
  `crates/pdfdelta-core/tests/line_fixture.rs`;
  `crates/pdfdelta-core/tests/pipeline_fixture.rs`;
  `crates/pdfdelta-bench/examples/layout_line_probe.rs`.
