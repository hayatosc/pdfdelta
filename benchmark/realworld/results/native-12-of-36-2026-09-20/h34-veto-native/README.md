# H34 first-veto probe (diagnostic only)

Temporary env-gated traces at: `recover_local_domain` real returns
(h34_local_veto), `prove_domain`/`assess` real record creations and the two
`close_domain` None returns (h34_trace). No new proof/charge work; 8 MiB cap;
I/O failures printed. One bounded IRS1099 capture, logical report
7964e3b5179c00edcc9a229b71bc0e9483c9975cf8204f92e51b06689a5773b2; core hook
restored byte-exact.

## Measured trace (1342 rows)

domain-record 452, child-record 484, local-veto 406 (adopted_equal 235,
not_source_bounded_or_incomplete 171), close-domain 0. The earlier
"845/642 evaluated_rejected_at_child" classification is retracted: it inferred
segment rejection from broad covering records and unrelated child samples. The corrected correlation (h34-correlate.json.gz) is relation-id
membership only: a child record is matched solely by its own id, the parent is
kept as annotation, and no cause share is claimed with their kind/relation/parent/label/detail.

## Block 32 (old [0,20), other_ids 0/34/68)

Read-only report stream (block32-probe.json.gz, block32-substring-probe.json.gz):
the unresolved region is one-sided (`new_span` null) with evidence
`reading_order_unknown`, page 1, 20 glyph sources, text "File with Form 1096.".
Substring search over report span text and the 58 new-side single-block
unresolved span texts finds the target only on the old side, in three records
(0, 34, 68) that repeat the same source text.

Measured bracketed-path guards (h34b-veto-native/h34b-trace.jsonl.gz, 10 rows,
binary abb7b55d..., logical report
7964e3b5179c00edcc9a229b71bc0e9483c9975cf8204f92e51b06689a5773b2):
- `wrapper_established` records the real pairs list (167 then 170 pairs).
- `view_with_block32`: old side0 Untrusted(32) source_bounded=true
  order_certified=true blocks=[32] range 0..20; new side1 Untrusted(32)
  source_bounded=true order_certified=true blocks=[32] range 0..11.
- `nearest_selected` twice with real values: candidate_bounds
  (499.721, 614.288, 573.777, 614.288), above_index 18/20 with above_old
  (512.649, 641.677, 572.499, 641.677), below_index 38/40 with below_old
  (501.22, 446.862, 501.22, 446.862).
- The first actual guard is `band_empty` (twice): the above and below anchor
  x-ranges do not overlap (below anchor is a point at x=501.22), so the band
  (max(min), min(max)) is empty. Anchors and source/order evidence exist; this
  is the band rule, not a missing anchor or missing source. The candidate block
  was visited and the hold happens before LocalDomain creation.
- Anchor index kind: the recorded `above_index`/`below_index` (18/38 in the
  first round, 20/40 in the second) are positions in the `anchors` vector, not
  block ids. Both rounds correspond to the same real pair list entries
  (old 28, new 28) and (old 72, new 73); the vector-position to pair mapping
  itself is not independently proven. Whether the below anchor is really a
  single glyph is not confirmed.

Correlation note: the H34 correlation now matches child records only by the
child's own relation id being present in the segment ids, keeping `parent` as
annotation; it is relation-id membership, not span-equality proof. The
H34b trace is stored under h34b-veto-native/.

