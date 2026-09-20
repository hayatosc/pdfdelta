# H5 precondition audit: no clean trusted candidate on accepted HEAD

Status: H5 as a standalone per-span exemption is **falsified for the named
beneficiary**. Its precondition (a candidate whose old and new spans are
internally proven and source-bounded) does not hold on the accepted source.
H5 remains a possible *addition* to the H4 candidate-discovery foundation, but
only as an explicitly dependent combined mechanism.

## Method

A first pass mapped candidates onto unresolved-region evidence labels. That
was circular: a coarse forced span over the whole window makes every subrange
appear to touch order evidence. The corrected method joins each candidate span
block to the *intrinsic* prepare-time provenance (per-block layout reason code,
uncertain/inferred flags, normalization issue count, trusted-run interval),
recorded with the temporary `PDFDELTA_H5_DEBUG` diagnostic on the accepted
source.

## Result (raw provenance, accepted HEAD)

| span block provenance | candidates |
| --- | ---: |
| all blocks trusted, not inferred, no normalization issues (`clean`) | 22 |
| blocks inside inferred order only | 80 |
| at least one intrinsically order-uncertain block | 8 |

The 110 candidates split into 22/80/8. So a clean trusted precondition does
exist: e.g. candidate 33 pairs old block 213 (`There `) with new block 215
(empty), and both blocks are intrinsically trusted and issue-free. Today the
reasons still name all four labels because `inspect_source_reasons` adds
`NormalizationUncertainty` when *any* block in the document has issues, and
`touches_barrier` uses coarse alignment-span ranges, so the single forced
window over the document intersects every key.

Example fragments whose blocks are all inside those regions:

- candidate 1/2: old block 77 (`.` ) / new block 88 (`icle`), evidence
  `reading_order_inferred` + `reading_order_unknown` on both blocks;
- candidate 4: old block 85 (`In addi`) / new block 96 (`Fur`), same evidence.

So the order labels are not unrelated document-global noise: they intersect
every candidate span. `domain_reasons` keeps them because the intersecting
alignment spans carry `ReadingOrderUnknown`/`ReadingOrderInferred`, which is
the correct conservative behavior on this source. Clearing the order label for
these spans would not be a gain, because the spans themselves are fragments of
order-uncertain or inferred regions, and `domain_not_closed` would still hold.

## Consequence for H5

- H5 has a real precondition set: 22 candidates whose span blocks are
  intrinsically trusted and issue-free. Clearing the propagated order and
  normalization reasons for exactly those spans is a sound per-span rule
  (equivalent to the existing local-domain preconditions), while the 80
  inferred-only and 8 crossing-uncertain candidates keep their barriers.
- Two propagated sources must be corrected with intrinsic evidence:
  `inspect_source_reasons` adding `NormalizationUncertainty` globally, and
  `touches_barrier` intersecting coarse span ranges instead of the key's own
  blocks. `domain_not_closed` remains a separate ownership question and must
  be proven by closure, not declared.
- The required negatives stay: a candidate touching or crossing an
  intrinsically uncertain block keeps the barrier (8 candidates), inferred
  order is not promoted to exact (80 candidates), a normalization issue inside
  the span keeps the barrier, and work/budget exhaustion fails closed.
- H4's fine-grained alignment output is a likely prerequisite for closure on
  the 22 clean candidates, but the intrinsic rule above must be justified by
  fixtures and real measurement before combining.

## Files

- `provenance-join.json` — per-candidate intrinsic provenance (trusted /
  inferred / uncertain / normalization issue counts), clean candidate details
  and crossing examples. The earlier circular `candidate-states.json` was
  removed; its downstream-label histogram is superseded by this join.
