# H5 precondition audit: no clean trusted candidate on accepted HEAD

Status: H5 as a standalone per-span exemption is **falsified for the named
beneficiary**. Its precondition (a candidate whose old and new spans are
internally proven and source-bounded) does not hold on the accepted source.
H5 remains a possible *addition* to the H4 candidate-discovery foundation, but
only as an explicitly dependent combined mechanism.

## Method

On the accepted HEAD (H4 removed), all 110 `change_candidates` of
`edpb-restrictions-v1-to-final` were mapped onto the unresolved regions that
contain their span blocks, and each block was classified by the evidence
recorded on those regions: `reading_order_unknown`, `reading_order_inferred`,
`normalization_issue`, `extraction_gap`, or none (`clean`).

## Result

Every candidate has **both** the `unknown` and `inferred` states on its span
blocks:

| span block states | candidates |
| --- | ---: |
| `inferred`, `unknown` | 110 |
| without `unknown` or `inferred` | 0 |

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

- Standalone H5 has no beneficiary and will not be implemented.
- H4 and H5 are dependent in one direction: H4's alignment-level recovery is a
  necessary prerequisite for any candidate whose spans can be called trusted.
  Because H4 alone produced no content coverage (its own rejection record),
  the combined mechanism may only be attempted together with a proof that the
  *post-H4* spans are trusted, source-bounded and free of intersecting
  order/normalization evidence, and with the required negatives (touching or
  crossing an uncertain block, repeated text inside an uncertain competitor,
  normalization issue inside the claimed span, evidence or work exhaustion).
- Until that proof is demonstrated, the accepted source stays unchanged and
  the next cause must be chosen from the remaining ranking rather than from an
  unproven exemption.

## Files

- `candidate-states.json` — full histogram and example candidates with their
  span blocks, texts and recorded reasons.
