# Remaining recovery evidence

The active execution contract is [the remaining plan](../../../docs/plans/remaining/PLAN.md).
The fixed 36-pair panel and historical annotations remain unchanged.
`registration.json` binds their hashes and the two common-text baseline observations.
The baseline remains 0/36 complete; no new G1, G2 or G3 gain has been demonstrated.

```sh
PYTHONDONTWRITEBYTECODE=1 PYTHON_UV=0 python benchmark/realworld/remaining/test_verify.py
PYTHONDONTWRITEBYTECODE=1 PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage registration
PYTHONDONTWRITEBYTECODE=1 PYTHON_UV=0 python benchmark/realworld/remaining/verify.py --stage final
cargo test -p pdfdelta-bench --example paint_trace_probe --locked
cargo run -p pdfdelta-bench --example paint_trace_probe --locked -- INPUT.pdf
```

Registration succeeds. Final intentionally fails while later-stage evidence and
adapters remain unfinished. The reducer's synthetic pass test is not yet an
end-to-end all-stage pass test; C1 remains open.

## Additional proof experiments

| Record | Observation | Consequence |
| --- | --- | --- |
| `paint-probe.json` | 63 successful documents among 72 bounded attempts; 30 pairs have both sides. No complete conservative projection matches. | Missing captures remain explicit; this does not establish impossibility of other proofs. |
| `paint-marker-probe.json` | W-2 matches all 11 pages after omitting text and marked-content operators; five other selected pairs do not. | A candidate for a full execution proof, not an inventory certificate. |
| `paint-state-probe.json` | Schedule C/SE have changed stroke widths, lengths or positions. | Exact projected-program equality fails; nearby shapes cannot substitute for text identity. |
| `local-order-probe.json` | Source extents and controls were located in retained native worker glyph tuples. | A glyph interval after the last paint still does not exclude text painted earlier inside the visual band. |

The diagnostic delegates PDF/content parsing to existing libraries. It does not
implement a PDF object parser, OCR or correspondence rules. Its source/executable
snapshots and bounded raw observations are retained under ignored
`benchmark/realworld/cache/remaining-paint-*` directories. The projected trace is
not a complete graphics-state interpreter: omitted text or marked-content
operators can affect clipping or visibility. Resource equality does not establish
source identity. No projection is consumed by production comparison or coverage.

The next local-closure hypothesis is to retain conservative bounds for every
non-text paint, reject unknown/intersecting bounds, and admit a native band only
when all paint is provably outside it and acquisition has no dependent issue.
This would preserve global inventory uncertainty and non-owning B semantics.
It still requires positive/adversarial fixtures and real-target replay.

The native extractor now retains conservative paint bounds. The new
`closed-native-paint-bounds-interval-v1` review convention accepts only a locally
closed source band, including both boundary glyphs, with no overlapping or
unbounded paint and no dependent text acquisition issues. These B reviews do
not discharge strict source ownership or document-wide text completeness.
The fixed real-PDF gains remain unmeasured for this change.
