# Native interval closure: initial real-document result

The same previously frozen paragraph-12 reference was evaluated after adding
page-local native source closure and separating accepted source-boundary decisions
from unfinished optional interior candidate enumeration. `runs.json` binds the
release executable, input PDFs, source reference, changed production files and
raw outputs. The executable was built from `3ab78ea` plus the recorded source
changes with the committed lockfile, before this result record was committed.

Both shared routes still recover **0/1 annotated scope targets**, with zero B or C
predictions. Precision is undefined; strict event and position metrics are unknown
for this scope-only reference. After removing only `comparison_wall_time_ms`,
their entire JSON results equal the initial P3 results at `c4354d9`. This includes
strict coverage, unresolved dependencies, candidate decisions and empty review
arrays. `shared-contracts.json` records the comparison and canonical digests.

The global completeness gate was not the only blocker. The existing accepted
one-to-one literal boundaries do not enclose the inspected body paragraph. Its
old source reconstruction also retains unresolved normalization. The new closure
does not reinterpret unmatched text or resolve normalization by minimizing diff
cost. These results do not demonstrate expanded real-document recovery; the
remaining development references and independent-publisher positives are required.

| Route | Seconds | Peak RSS KiB | Exit | Report bytes |
| --- | ---: | ---: | ---: | ---: |
| Native | 30.32 | 640252 | 3 | 791109475 |
| Shared text | 7.21 | 349244 | 3 | 522584 |
| Shared all | 8.20 | 348312 | 3 | 524449 |

These are single functional observations, not a speed claim. Native annotation
metrics remain unknown because its report exceeds the 128 MiB evaluation ceiling.
Peak RSS is process memory, not live heap instrumentation. Raw outputs remain
outside version control. Reproduce with `capture-comparisons.py`, the registered
pair `edpb-controller-processor-v1-to-v2-1`, default limit scale 1, and a distinct
output directory as documented by the initial result record.

Programmatic native fixtures separately exercise the positive closed horizontal
range, comparison reversal, reversed source/node storage, unrelated page addition,
unassigned interior glyphs, acquisition gaps, branching/inferred order, rotated
text, partial crop and invisible paint. A complete graph-order flag cannot bypass
the native source scan. These tests establish the finite retained-text convention;
they do not replace actual PDF recovery or cover native cross-page intervals.
