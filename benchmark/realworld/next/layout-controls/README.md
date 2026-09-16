# Independent content and layout controls

These 60 authored pairs cross two existing PDF producers, five content states
(unchanged, number, unit, negation and affiliation) and six presentation states
(plain, wrapping, page break, font size, columns and paint order). They are
controlled fixtures, not natural documents or blind samples. The generator is
`crates/pdfdelta-bench/examples/next_layout_fixtures.rs`.

The manifest and source expectations precede comparison output. All 490 literal
selectors resolve uniquely. A wrapped line uses separate literal pieces rather
than inserting a nonexistent source space. The classic-xref producer uses TJ
positioning for word gaps: its literal quotes contain no space glyphs. The failed
initial attempt to match authored spaces is retained in `annotation-attempts.json`.
No source normalization is applied by the resolver.

Twelve number-change cases have one exact event and two side-qualified source
atoms as gold. Other changed paragraphs have finite content-change targets but
no unique internal-edit gold. The remaining paragraphs are unchanged controls.
The generator's canonical paragraph offsets are not raw-source offsets; the
resolved atom lists in `expectations.json` are authoritative for scoring.

Twenty-two pages cover every layout with unchanged content and all four content
changes in columns. Their render metadata and visual inspection are retained.
The two producers have identical full worker responses for these 11 combinations,
despite different space-glyph representation. This equality checks these fixture
bytes only. A changed paint order preserves visual positions; it is distinct from
renaming indirect PDF objects in the real-source mutations.

PDFs and rendered pixels remain in `benchmark/realworld/cache/next-layout-r2/`.
Generate into a fresh directory using the registered Schedule C source hashes:

```sh
cargo run -p pdfdelta-bench --example next_layout_fixtures -- /tmp/layout-fixtures \
  --old benchmark/realworld/cache/next-dev/irs-schedule-c-2024-to-2025-old.pdf \
  --new benchmark/realworld/cache/next-dev/irs-schedule-c-2024-to-2025-new.pdf \
  --old-sha256 f56bfd48f3604fc015b7ea22a70c6c36535a723ba84a2b9a961bc9838d070ce6 \
  --new-sha256 ddf401dbe060467d39f90ad2abf645df1de31512821a150dc68a3882bbf19716
```

Resolve each annotation using `pdfbench validate-literal-selectors --annotation
<annotation.json> --old <old.pdf> --new <new.pdf>`. The source mutations in the
manifest have separate full-page render checks and literal resolution. Renaming
objects preserves all original page responses, and prepending a page preserves
them at the shifted indices while adding an entirely white page. The reverse
reference swaps input sides and fixes the numeric change from 5 to 4.

## Paired results

Both revisions capture all 189 attempts (63 pairs, three separate routes).
All 189 existing result contracts agree after excluding wall time, additive
scope-review fields and search counters. No deadline evidence is discarded.
Results are in `results/`; raw reports remain in the cache. Complete comparisons
on the 60 authored pairs are 32/60 native, 10/60 text and 0/60 all-channel on
both revisions. All three real metamorphic pairs remain incomplete on each route.

| Metric on authored controls | Baseline | Current |
| --- | ---: | ---: |
| Native numeric strict events | 10/12 | 10/12 |
| Native numeric changed source atoms | 20/24 | 20/24 |
| Native numeric event/source precision | 1.0 / 1.0 | 1.0 / 1.0 |
| Shared strict numeric events, each route | 0/12 | 0/12 |
| Shared B exact-range detections, each route | 0/48 | 10/48 |
| B source-range precision | undefined | 1.0 |
| B extra context source atoms, each route | 0 | 0 |
| Scope C, each route | 0 | 0 |

The two missed native numeric cases change paint order. Ten other native strict
events concern affiliation changes without unique internal-position gold; they
are not added to the numeric precision denominator. Shared strict precision is
undefined because there are no typed changes. There are no strict changed-source
intersections with any annotated unchanged paragraph on either revision.

The ten new B reviews exactly match the preregistered changed paragraph source
sets: unit changes in plain and font-size layouts, and negation changes in plain,
font-size and wrapped layouts, each from both producers. They retain separate
non-owning review status. No B is recovered for column, page-break or paint-order
changes. Native numeric changes in columns are recovered, while unchanged column
and page-break controls have complete native and text comparisons. These outcomes
evaluate the positive and negative cases without redefining B as strict recall.

Both revisions produce 52 legacy text proposals per shared route. Forty-eight
match the authored content targets when only ASCII word gaps are disregarded;
four concern unchanged prose wrapped by the lopdf producer. This is an explicit
visible-content quality convention for these authored controls, not literal
source normalization or a correspondence proof. All-channel reports additionally
contain 56 non-text proposals, whose quality is unscored. These predictions are
not added to A or B recovery. Per-presentation stage counts and denominators remain
in `summary.json`, with detailed coverage in `scores.json`.

Each real metamorphic pair retains its one exact annotated numeric event and both
changed source atoms in native comparison (3/3 events, 6/6 atoms, precision 1.0).
The other seven events per pair remain outside the partial gold. Separately,
all eight native event kinds and occurrence source sets match the original
Schedule C comparison after reversing insertion/deletion and sides where needed.
Object IDs and deliberately shifted page indices are excluded from that identity
check. Shared routes miss all three numeric targets; their six all-channel
proposals remain unscored. No false strict atoms intersect the unchanged titles.

Total process wall times are 15.11 seconds at baseline and 15.38 seconds currently;
maximum process RSS is 35,168 versus 35,492 KiB. Outputs total 174,703,356 versus
175,108,800 bytes. Revisions were captured sequentially, but these single samples
do not establish a PDF-wide speedup. RSS is not allocator live memory and review
time was not measured.

Reproduce captures and scoring in fresh destinations:

```sh
PYTHON_UV=0 python benchmark/realworld/next/layout-controls/capture.py \
  /path/to/baseline-pdfdelta /tmp/layout-fixtures /tmp/layout-baseline \
  --implementation 3f318d7
PYTHON_UV=0 python benchmark/realworld/next/layout-controls/capture.py \
  /path/to/current-pdfdelta /tmp/layout-fixtures /tmp/layout-current \
  --implementation 9093cab
PYTHON_UV=0 python benchmark/realworld/next/layout-controls/score.py \
  /tmp/layout-baseline /tmp/layout-current /tmp/layout-scores
```

The reverse reference uses the registered original PDFs in `next-dev`; generated
mutations are read from the supplied fixture directory. Scoring rejects modified
reports and unhandled shared typed changes. The source
manifest's `comparison_performed: false` describes registration time, not the
subsequent state of this results directory.
