# Common-revision observations for the two early pairs

The controller/processor guideline and NIST SHA standard were initially measured
on intermediate revisions. These captures supply the immutable `3f318d7` baseline
and `9093cab` release observations needed for a common development aggregate.
They reuse the previously fixed input hashes and source references. Earlier
attempts remain in their original directories; these are not additional pairs.

All twelve process attempts capture incomplete reports with exit 3. Both native
reports are byte-identical between revisions. Native returns zero strict events
for the guideline and nine for NIST; the nine lie outside the partial source
reference and remain unscored. No strict changed source intersects either frozen
positive target or the annotated unchanged controls. Neither reference supplies
unique internal strict event/position gold.

Both shared routes miss both frozen paragraph targets (0/2). Strict typed changes
and scope C are zero. The current NIST reports retain eight B reviews per route;
the guideline has none. Every NIST review's operation text and old/new interior
source counts match the previously adjudicated eight units. Their posthoc visible
content precision remains 6/8, with the whitespace-only and glyph-mapping cases
retained as limitations. The routes repeat the same predictions. They are not
sixteen independent recoveries or part of the frozen recall denominator.

Shared contracts match after removing only elapsed time, additive review fields
and the search counters `index_entries` and `token_visits`. Native fields were
selected from the complete captures by reading top-level `summary`, `changes`
and `proven_changed_regions`, avoiding the former 128 MiB summary ceiling.
Run records retain full-report hashes, sizes, process time, peak RSS, input hashes
and executable hashes. Process RSS is not live heap memory; no speedup or human
review-time claim follows from these functional observations.

Reproduce with the existing `capture-comparisons.py` driver, pairs
`edpb-controller-processor-v1-to-v2-1` and `nist-sha-1803-to-1804`, the recorded
binary and input hashes, default limit scale 1, and a fresh destination per build.
For native scoring, intersect each occurrence's side-qualified glyph IDs with
the frozen literal reference atoms. For shared scoring, use the non-owning
review source sets and keep the prior posthoc judgments separate.
