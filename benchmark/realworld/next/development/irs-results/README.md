# IRS form evaluation

Four registered pairs have source references fixed before comparison: W-9,
English W-4, Schedule C and Schedule SE. Each reference contains one changed
first-page text range and an unchanged title control. All 16 literal selectors
resolve uniquely; the eight source inspections report complete extraction.
Both first pages were visually checked before comparison. These partial
references do not provide exhaustive whole-document precision or recall.

The selected W-4 year and Schedule C participation-question year each have one
unique replacement, `4` to `5`, with two changed source atoms across both sides.
Native detects both exactly: restricted event recall 2/2, changed-source recall
4/4, and no extra changed source atoms inside either selected range. Restricted
event and source precision are both 1. The baseline already has these detections.
W-9 and SE have range-content gold without a unique internal edit-history gold.
Native changes intersect the SE range but miss the W-9 range; this intersection
is not counted as a B scope detection. Every selected unchanged title has zero
false strict changed sources.

| Pair | Native strict events | Legacy native proven regions | Shared text A/B/C | Shared all A/B/C |
| --- | ---: | ---: | --- | --- |
| W-9 | 14 | 6 | 0 / 0 / 0 | 0 / 0 / 0 |
| W-4 | 7 | 2 | Failed | Failed |
| Schedule C | 8 | 0 | 0 / 0 / 0 | 0 / 0 / 2 page renders |
| Schedule SE | 10 | 1 | 0 / 0 / 0 | 0 / 0 / 2 page renders |

Every entry applies to both revisions. Native reports are byte-identical, and
their unannotated predictions remain unscored. Legacy proven regions are kept
separate from the shared B contract. Shared routes detect none of the four frozen
text-scope targets. Schedule C strict numeric recall is 0/1 events and 0/2 source
atoms. W-4 contributes another missed target to end-to-end recall because it
fails before emitting a report; its output precision is unavailable. Zero strict
detections on captured routes have undefined precision.

## Inferred page comparisons

The four all-channel C results compare the same numbered page and form part.
All four page pairs were visually checked and contain visible changes. On the
second Schedule C page, the year, Part V references and line 48 reference change.
The second SE page changes income and profit amounts. First-page changes include
the preregistered year and amount targets, but whole-page proposals receive no
strict or text-scope recall credit.

These are posthoc judgments, giving page-pair correctness 4/4 and visible-change
precision 4/4 for this small development sample. They are existing baseline
results, not additional recovery. They do not establish inferred text-scope
quality. Pixel-level precision remains unmeasured. Each proposal exposes a
612 by 792 review area; changed-pixel counts are 4,474 and 695 for Schedule C,
and 2,198 and 8,948 for SE. The retained masks exclude the rest of those pixels.
Human review duration and minimum necessary context were not measured.

## Costs and completion

Twenty of 24 process attempts capture reports, all incomplete with exit 3.
Both W-4 shared routes fail with exit 2 on both binaries: descendant ownership
exceeds the unchanged 1,000,000 limit. The failure is retained as a scope-stage
resource limit. No budget increase or replacement removes it from the ledger.
Per-scope enumeration, optimization, comparison counts and channel source
coverage are retained in `scores.json`; a captured report is not a complete
comparison. Literal resolution is 16/16 and inspected source acquisition is 8/8,
separate from complete shared-channel inventories and whole-document coverage.

P1 reduces text-search token visits from 33,725 to 8,381 for Schedule C and
31,321 to 11,167 for SE. Shared results match after excluding only wall time,
additive review fields and those search counters. Native bytes match exactly.
Single-sample baseline/current text times are 0.28/0.28 seconds for Schedule C
and 0.24/0.25 seconds for SE. These observations support reduced kernel work,
not a PDF-wide speedup. All run times, peak RSS, failures, output sizes and
executable hashes are in the run records. RSS is not allocator live memory.

The native output sizes range from 36.7 to 343.2 MB; raw reports stay outside
the repository. `native-selected-fields.json` preserves the original summary,
strict changes and legacy proven regions without duplicating the large alignment
records. `contracts.json` binds report parity. The inferred adjudication retains
source-render hashes, dimensions, zero warning flags and comparison pointers.

## Reproduction

Build each recorded revision and verify the frozen input and executable hashes.
Use a fresh output directory for each binary:

```sh
PYTHON_UV=0 python benchmark/realworld/next/development/capture-comparisons.py \
  target/release/pdfdelta benchmark/realworld/cache/next-dev /tmp/irs-replay \
  --pair irs-w9-2018-to-2024 --pair irs-w4-english-2024-to-2025 \
  --pair irs-schedule-c-2024-to-2025 --pair irs-schedule-se-2024-to-2025 \
  --implementation YOUR_BUILD_COMMIT
```

Resolve each annotation using `pdfbench validate-literal-selectors` with its
registered old and new inputs. Score strict numeric gold by side-qualified glyph
IDs from native change occurrences, not by matching digit text alone. The
unchanged title controls use the same source intersections. No production code
changes in this evaluation; the remaining development and blind work is pending.

Validation: formatting and workspace clippy pass; 2,326 workspace tests pass
with two measurement entry points intentionally ignored.
