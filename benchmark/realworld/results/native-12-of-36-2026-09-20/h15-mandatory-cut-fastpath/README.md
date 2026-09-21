# H15 mandatory-cut fast path: rejected for the measured W4 stall

Status: rejected before implementation. A bounded trace of the current H14
binary on `irs-w4-english-2024-to-2025` recorded every boundary proposal's
parent key, group lengths, remaining budget, whether H13's mandatory-match
analysis was already cached, the fixed localized cuts, and whether each cut is
adjacent to a mandatory equal edge (`w4-boundary-trace.txt`, 13 rows).

Findings:

- The fatal wide-domain key `(471,614)/(559,702)` (lengths 1021/1021) has the
  H13 analysis cached from forced-equal siblings, but its fixed cuts
  `152..157 / 110..115` are neither parent endpoints nor adjacent to mandatory
  edges (`fixed = Some((false, false, false))`).
- Across all 13 traced proposals no row has both cuts fixed; three rows have
  exactly one fixed cut, one of them with equal slices.
- The shape required by the hypothesis (both cuts forced through mandatory
  vertices with unequal child slices) does not occur in the current W4 stall,
  so the sufficient positive fast path cannot replace the exponential
  enumeration there.

Decision: no production change. The temporary trace was removed after
capturing the evidence (`PDFDELTA_H15_TRACE` absent from the tree). H13/H14
gains and the 3/36 score are unchanged; the next cause should target the
actual stalled work visible in the trace rather than cut adjacency.
