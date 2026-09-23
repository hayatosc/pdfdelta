# H35 bracketed first-veto distribution (diagnostic only)

Temporary env-gated traces in `views::discover_bracketed_domains`,
`nearest_boundaries` and the `local.rs` wrapper, no new proof/charge work,
8 MiB cap. One bounded IRS1099 capture (H34B trace): 1786 rows, binary
28e48328e39c47c85f086acfef694475e38a80edb9a6a2e2a3645f509dda2fa4, logical
report 7964e3b5179c00edcc9a229b71bc0e9483c9975cf8204f92e51b06689a5773b2,
no truncation and no diagnostic I/O errors. Core hooks restored byte-exact.

## Terminal first-veto distribution (H33 census side0, 1044 residual tokens)

Both wrapper rounds are identical (one block/round first terminal):

| terminal label | tokens |
| --- | --- |
| view_not_untrusted_or_source_bounded (all blocks of that view) | 525 |
| old_unique_other_or_none | 271 |
| no_nearest_boundaries | 228 |
| band_empty | 20 |

Terminal tokens 1044, unknown 0; total old blocks 294, terminal-carrying blocks
(as recorded, including view-level labels) 294, true residual blocks 49.
Bindings are recorded in h35-aggregate2.json.gz (trace, patch, census and
report file SHA256). A view-level rejection applies to every block inside that view
for this pass and is not a cause for other passes. `tail_continue_N` rows are
deliberately excluded from the terminal set (order-only guard identity); they
did not contribute here.

## Block 32 bracket-selection finding

`nearest_anchor_scan` (86 scan rows): 42 scan rows lie below the candidate
(y < 614.288). Four rows intersect the above anchor x-range
[512.649, 572.499] with real width; they are two distinct block pairs seen in
both rounds:
- anchors 42/44: x 50.4..566.74, y 410.256, 36.606 below the chosen point;
- anchors 41/43: x 405.903..572.244, y 422.503, 24.359 below the chosen point.

The chosen below anchor (y 446.862, x point 501.22) yields an empty band, while
farther below anchors with above-x overlap exist. Correction: the four scan
rows are two distinct block pairs (anchors 42/44 at y 410.256 and 41/43 at
y 422.503) appearing in both rounds, not four independent candidates. The
earlier statement that this evidence supports a closed-empty-side fix is
retracted: empty-side handling was not measured at all. Limits: the x overlap
test used the above anchor x-range only, and the below-anchor single-glyph
question remains unconfirmed.

Files: h35-trace.jsonl.gz, h35-hook.patch.gz, h35-aggregate2.py.gz,
h35-aggregate2.json.gz, h35-aggregate2.log.gz, capture.log.gz, summary.json.gz,
binding.json.gz.

## H36 next step (source-evaluated, not implemented)

`views::build_views` always sets `source_bounded: false` for Trusted views, but
computes `block_bounded -> block_candidate -> block_candidates` separately, and
`positioned_block(view, block)` is exactly that per-block candidate. The
525-token view rejection is therefore not evidence of missing provenance.

Held: a singleton-only admission (30 tokens) is too small to matter.
Next unmeasured item: the 495 multi tokens' members and their actual
`positioned_block` qualification, measured by adding only
`view.block_candidates` (Vec<bool>, already computed in `build_views`) to the
existing view dump in the next capture. Closed-empty or joint bracket selection
is not asserted as the main path.
