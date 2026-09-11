# Frozen blind outcome

All 72 attempts produced reports: 12 pairs, three routes, two revisions. The
baseline ran before the current executable, with no concurrent measurement jobs.
Implementation, executables, limits, selected series and source annotations were
frozen before comparison. No production or comparison-configuration changes
were made after observing these results.

The blind set shows **no additional B recovery**. Both shared routes recover
0/12 frozen scope targets, including the kana target with unresolved native
annotation. All captured reports remain incomplete. This limits generalization
of the independently adjudicated development recovery; it does not invalidate
the registered blind attempts or justify removing difficult families.

## Accuracy and proposals

The table applies to both revisions. Each family has two registered pairs and
two attempted changed-content targets. Shared B, scope C and legacy inferred
changes are zero on both shared routes in every family.

| Registered family | Native A records | Native tentative edits | Shared B target hits | Restricted strict event / source recall |
| --- | ---: | ---: | ---: | --- |
| ID-free prose | 0 | 241 | 0/2 | undefined: no exact position gold |
| Heading/move/list/copy | 0 | 41 | 0/2 | undefined: no exact position gold |
| Table/form | 5 | 31 | 0/2 | 0/1 event; 0/2 side-qualified glyph atoms |
| Multicolumn | 10 | 19 | 0/2 | undefined: no exact position gold |
| Japanese/vertical/rotated/ruby | 0 | 0 | 0/2, one unresolved | undefined: no exact position gold |
| Clip/overpaint/figure/scan/mixed | 0 | 393 | 0/2 | undefined: no exact position gold |

The exact reference is the selected W-2 Copy A year. Native, shared text and
shared all-channel routes all miss that one event and both source atoms. There
are no predictions within that restricted gold, so event and source precision
are undefined. The 15 native A records concern other locations and remain
unscored, rather than being counted as false positives or successful recall.

All-channel W-2 additionally reports 20 source form-value changes on both
revisions. The local operations change an empty field value to the name `Off`.
These are not the annotated printed-year change and do not imply a visible
checkbox change. Shared text has zero A records. Whole-document event precision
is unknown; the partial gold does not cover every reported operation.

The 725 native tentative edits retain their assumptions and uncertainty. Two
touch one side of a frozen FAA target; none touches both selected sides, and none
recovers the W-2 numeric target. Restricted tentative numeric event/source recall
is therefore also 0/1 and 0/2, with undefined precision. The quality of unannotated
tentative edits is unknown. Native tentative edits, native proven regions, shared
B and shared C are separate populations and are never added to strict recall.

No strict mask intersects any of the 440 unchanged control glyph atoms per
route/revision. This is a finite negative check, not complete comparison of those
paragraphs. There are no B or scope-C predictions to score for range precision;
their precision is undefined and predicted excess context is zero. No human
review-time reduction is claimed.

## Completion and failures

All 24 active PDF downloads are hash-verified. Literal annotation resolves for
11/12 pairs and 34/36 selectors. The source resolver reports complete extraction
for 12/24 input sides; this is not a visible-content completeness certificate.
Native comparison reports complete extraction on both sides for 3/12 pairs.

| Shared stage, completed / 12 attempted per revision | Text | All channels |
| --- | ---: | ---: |
| Selected channel inventories | 0/12 | 0/12 |
| Candidate enumeration | 3/12 | 1/12 |
| Conflict search | 11/12 | 11/12 |
| Conflict search and every optimization component | 11/12 | 11/12 |
| Complete channel source comparison | 0/12 | 0/12 |
| Overall comparison | 0/12 | 0/12 |

These counts agree across revisions. Enumeration of an empty retained population
does not discharge failed acquisition. NIST risk-management and controls text
reports illustrate this: resource-limit acquisition issues precede zero selected
local comparisons. Kana likewise has zero native glyphs but visible source text.
Nine text routes have incomplete text candidate search; NIST contingency also
exhausts a conflict component's search budget. Individual extraction issue kinds,
competing optima and scope reasons remain in `pairs.json`.

Common acquired source inventory and native supported-text extraction have
different contracts. Native enumeration/optimization/source-comparison stage
flags are not exposed by this capture and remain unknown. No process hit its
180-second timeout; internal resource limits and unsupported extraction still
prevent complete results. All 72 attempts remain in the denominator.

## Costs and reproducibility

| Route | Baseline seconds | Current seconds | Baseline max RSS KiB | Current max RSS KiB |
| --- | ---: | ---: | ---: | ---: |
| Native | 117.84 | 136.36 | 3,538,400 | 3,562,876 |
| Shared text | 116.13 | 115.14 | 716,940 | 716,572 |
| All channels | 115.68 | 116.87 | 716,916 | 715,232 |

Times are sums of twelve single process observations per route. Total observed
time rises from 349.65 to 368.37 seconds. This is not evidence of PDF-wide speedup
or a controlled estimate of slowdown. Peak RSS is process resident memory, not
allocator live heap. Output totals are 15,347,147,169 and 15,347,150,762 bytes;
the SP 800-53 native report alone exceeds 10 GB. Large retained region evidence
is a practical reporting cost even though the process finishes within its limit.

`baseline-runs.json` and `current-runs.json` retain binary/input/reference/report
hashes, exits, times, RSS and report sizes. `pairs.json` and `families.json` preserve
separate units, attempted/observed denominators and null precision. `sources.json`
binds the scorer, frozen annotations and result files. See `../README.md` for
input acquisition, replay and scoring commands. Raw PDFs, reports and rasters
remain in the ignored cache. No new comparison runs are required to rescore.
