# Applicability of paint outside the page rectangle

## Decision

Do not prioritize excluding page-exterior paint as a route to natural inventory
completion. All 72 fixed inputs have retained paint that touches or overlaps the
page rectangle, or has unknown bounds. No observed paint-bearing page contains
only strictly exterior records. This is an applicability measurement; production
inventory, source coverage, search, and completion are unchanged.

## Measurement

The frozen `domain-content-v1/pdfdelta` executable acquired each input through
the native worker without an extraction cache. The driver verified every panel
input hash, used the original request defaults and a 35-second process timeout,
and received a successful typed response for all 72 inputs. Acquisition success
does not mean complete extraction: the responses retain 18 issues in total.

The worker's canonical page rectangles and retained non-text paint bounds are in
the same coordinates. A record is exterior only when its finite rectangle is
strictly separated on at least one axis. Boundary contact remains unresolved;
unknown bounds, absent page metadata and invalid rectangles remain unknown.
Six direct classification controls cover separation, overlap, contact and invalid
bounds. This does not validate a new renderer or independently prove every
producer's geometric bounds.

| Retained paint classification | Records |
| --- | ---: |
| Contact with or overlap of page rectangle | 403,130 |
| Unknown bounds | 42,347 |
| Strictly outside page rectangle | 841 |

There are 4,965 paint-bearing pages. Exterior records occur in five documents;
zero pages have exclusively exterior records. These are retained worker records,
unlike the earlier empty-clip probe's pre-rollback execution attempts. Do not
compare their totals as if they used the same counting convention.

The observation neither classifies visible paint as text or decoration nor
discharges OCR/outline interpretation. It also does not prove that the overlapping
records are visible: clip shape, masking, opacity, background and subsequent
compositing remain separate dependencies. Removing exterior records alone would
not establish any new complete page inventory in this panel.

## Evidence and scope

`audit_paint_extent.py` is a read-only benchmark driver.
`paint-extent-results.json` binds its hash, executable, panel, inputs, request,
response hashes and retained projections. Each projection preserves all page,
inventory, issue and paint observations needed for this classification under
`benchmark/realworld/cache/source-completion/paint-extent-v1/`. Full glyph payloads
are not retained by this diagnostic and are not used to claim text comparison.

No production code changed, so the preceding production checks remain applicable.
The diagnostic completed all inputs and its six classifier checks; diff checks
pass. This is not a rerun of the 36 comparisons. No completed pair is added, and
the active goal and temporary plans remain.
