# H34b bracketed-path trace (diagnostic only)

Temporary env-gated traces in `views::discover_bracketed_domains` (guards,
block32 view dump, nearest selection), `nearest_boundaries` (block32-only
reason labels) and the `local.rs` wrapper (real collect_established_blocks
pairs). No new proof or charge work; 8 MiB cap; I/O failures printed.

One bounded IRS1099 capture: 10 trace rows, binary
abb7b55d94e593d0b89d93ad25c618987c6a62e92774e27e4dbc6d2c2ad2acf2, logical
report 7964e3b5179c00edcc9a229b71bc0e9483c9975cf8204f92e51b06689a5773b2
(matches H32), no truncation and no diagnostic I/O errors.

Measured: real established pairs 167 then 170; block32 view on both sides
(Untrusted(32), source_bounded=true, order_certified=true, old range 0..20,
new range 0..11); `nearest_selected` with candidate_bounds
(499.721, 614.288, 573.777, 614.288), above_index 18 (above_old
512.649..572.499 at y 641.677), below_index 38 (below_old point x 501.22 at
y 446.862), then `band_empty` twice. The first actual guard is the x-band
overlap rule; anchors and source/order evidence exist. The candidate block was
visited and held before LocalDomain creation.

Index kind note: above/below indices are anchor-vector positions, not block
ids; the below anchor being a single glyph is not confirmed.

Core hooks restored byte-exact. This is diagnostic only: score remains 3/36.
Files: h34b-trace.jsonl.gz, h34b-hook.patch.gz, capture.log.gz, summary.json.gz,
binding.json.gz.

All runs used the bounded wrapper (6 GB aggregate, swap 0, one job); parent
`pdfdelta-heavy.slice/memory.events` stayed low 0 / high 0 / oom 5 / oom_kill 4
(unchanged). The Rust tree is byte-identical to the H32 final source, so the
five gates from `h32-suffix-full-iteration-017-native/gates.txt` are reused
rather than re-run.
