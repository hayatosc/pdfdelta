# DDPM and BERT source evaluation

Two registered development pairs were annotated before either comparison route
ran. DDPM has one changed introductory paragraph and an unchanged abstract
sentence on its first page. BERT has one changed abstract paragraph, an unchanged
abstract sentence, and a restricted numeric target: the final digit of the GLUE
score changes from 80.4% to 80.5%. All ten literal selectors resolve uniquely.
Both first pages were visually inspected before comparison. The selected DDPM
body is single-column with images; BERT is two-column. Neither sampled target
crosses a page boundary. Family registration labels do not establish those traits.

## Before and after the acquisition refinement

All 18 process captures across the immutable baseline, pre-fix implementation and
page-gap implementation return exit 3. Each route remains incomplete. The two
preregistered paragraph targets remain missed, and the restricted BERT numeric
target has event recall 0/1 and changed-source recall 0/2. The unchanged sentence
controls have no falsely changed sources. These are partial references, not
exhaustive document recall or precision denominators.

| Pair and route | Baseline strict events | Current strict events | Baseline scope reviews | Current scope reviews |
| --- | ---: | ---: | ---: | ---: |
| DDPM native | 0 | 0 | Not this route's contract | Not this route's contract |
| DDPM shared text | 0 | 0 | 0 | 39 B, 0 C |
| DDPM shared all | 0 | 0 | 0 | 39 B, 0 C |
| BERT native | 6 | 6 | Not this route's contract | Not this route's contract |
| BERT shared text | 0 | 0 | 0 | 0 B, 0 C |
| BERT shared all | 0 | 0 | 0 | 0 B, 0 C |

BERT's six native strict events and three legacy proven regions lie outside the
partial reference and remain unscored; their overall precision is unknown, not
zero or one. The retained selected native fields preserve their exact source
locations. Zero strict detections on the other routes have undefined precision.
Legacy tentative candidates and proven regions are not added to strict recall
or merged with shared B/C counts. Text and all routes repeat the same 39 reviews;
they are not 78 independent recoveries.

DDPM's failed Form runs on zero-based page 1 before its first retained glyph.
Its neighbors lie on pages 0 and 1. Keeping the extractor's known page prevents
this gap from marking every other page incomplete. The failed page and complete
document remain unresolved. The production change and tests are described in
`../../P4-page-gaps.md`; no filename, reference label or expected annotation
participates in the condition.

## Post-comparison review of the 39 additions

All 39 returned intervals were checked against source projections and seven
rendered pages: old pages 8–10 and new pages 8–11, using zero-based indexes. The
39/39 visible-content-change and interval-correctness judgments are posthoc
development precision observations, not preregistered or blind recall.

One interval is an ID-free funding paragraph bounded by unchanged headings.
Its final sentence changes the computing-resource acknowledgment. This provides
additional source-traceable B recovery from the Berkeley producer, independently
of the earlier NIST body examples. The other 38 intervals contain changed
bibliographic labels, additions, reordered references or publication details.
Some begin or end within a reference: the contract compares finite intervals,
not the identity of every reference or the author's edit history.

The funding review contains 142 old and 149 new source scalars. Its unchanged
first sentence contributes 94 context scalars on each side, none of which enter
the changed masks. This context measurement is restricted to that inspected
paragraph. Review extent, source IDs, bounding boxes and the literal projection
hashes are retained in `posthoc-adjudication.json`. Human review duration and
aggregate excess-context gold for the bibliographic intervals were not measured.

The static review bundle contains 39 B rows, 78 source-PDF links and 49 retained
page previews, totaling 95,625,616 bytes. Its comparison JSON matches the measured
report after removing only wall time. `static-review.json` binds the artifact;
the large bundle remains in `/tmp/pdfdelta-ddpm-page-gap-review`. Generate it with
the two DDPM inputs, `--channels text --review NEW_DIRECTORY`. No additional
browser timing or interaction claim is made for this unchanged output interface.

## Contracts, costs and limits

All native bytes match the baseline after removing only DDPM's two new failure
page fields. Shared results match after excluding execution time, additive
review fields, the indexed-search counter and the refined failure-page metadata.
Operations, accepted correspondences, existing masks, coverage and completion
remain equal. The full normalized hashes are in the contract records.

DDPM text output grows from 1,610,870 to 4,769,577 bytes because it now contains
39 complete non-owning review records. Its pre-fix/current process observations
are 3.74/6.04 seconds and 144,800/152,704 KiB peak RSS. All-channel observations
are 3.67/7.70 seconds. Other unchanged routes also ran slower during this capture;
some runs overlapped workspace checks. These single samples preserve costs and
regressions, but cannot establish a controlled timing effect or speedup. RSS is
not allocator live memory. Raw reports total hundreds of megabytes each for the
native route and remain outside the repository.

Adopt the source-page refinement: it repairs lost acquisition provenance while
preserving gap dependencies and strict results, and enables independently
checked B output. It does not repair the frozen first-page targets, native
cross-page closure, column-change coverage, remaining development references or
blind evaluation. Those remain required work.

## Reproduction

Build the CLI from the corresponding revision, verify the registered input
hashes, and run each binary into a fresh destination:

```sh
cargo build --release -p pdfdelta-cli
PYTHON_UV=0 python benchmark/realworld/next/development/capture-comparisons.py \
  target/release/pdfdelta benchmark/realworld/cache/next-dev /tmp/arxiv-replay \
  --pair arxiv-ddpm-v1-to-v2 --pair arxiv-bert-v1-to-v2 \
  --implementation YOUR_BUILD_COMMIT
target/debug/pdfbench validate-literal-selectors \
  --annotation benchmark/realworld/next/development/annotations/arxiv-ddpm-v1-to-v2.json \
  --old benchmark/realworld/cache/next-dev/arxiv-ddpm-v1-to-v2-old.pdf \
  --new benchmark/realworld/cache/next-dev/arxiv-ddpm-v1-to-v2-new.pdf
```

Repeat literal resolution with the BERT annotation and inputs. To inspect the
large native report's retained subset, select `summary`, `changes` and
`proven_changed_regions` with a JSON processor. These are existing report fields;
the capture does not introduce another comparison path. Metadata binds the
actual executable, source files, annotations and reports. The comparison runner
records caller-supplied build labels; source hashes make the working-tree build
explicit rather than pretending it was already an immutable commit.

Validation: formatting and workspace clippy pass; 2,326 workspace tests pass,
with two measurement entry points intentionally ignored. All 48 generated
renderer/acceptance cells pass, including the five required core cases.
