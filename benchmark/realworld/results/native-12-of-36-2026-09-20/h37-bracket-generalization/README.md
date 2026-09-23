# H37 bracketed whole-member generalization (rejected candidate)

Candidate: generalize `discover_bracketed_domains` from untrusted source-bounded
singletons to whole original members of trusted/untrusted views, with member
lookup on both sides, a same-view relative member order guard, and no
view-level `source_bounded` promotion. Patch `h37-views.patch.gz`
(sha256 in binding.json.gz), candidate binary 2ce761e7.

Result on 6 pairs vs H32 full017: 1099/schedule-c/SE summaries identical,
FAA/bunka logical reports identical, **W2 regressed** (old/new resolved
15847 -> 15095, unresolved 267 -> 238, retention fail 4 problems). Exact
ownership difference (per-block token union/subtract, h37-w2-loss.json.gz):
old and new each lost exactly 752 tokens with 0 gained.

Cause evidence (traces; common observation points equivalent, candidate additionally traced tail_stop/exit, baseline only entry; both patches archived):
| label | baseline | candidate |
| --- | --- | --- |
| bracket_entry r1 | 23,764,631 | 23,764,631 |
| bracket_exit r1 | 20 domains, 22,220,350 | 33 domains, 21,747,406 |
| bracket_entry r2 | (none) | 21,732,169 |
| bracket_exit r2 | (none) | 33 domains, 19,712,888 |
| tail_entry | 6,977,179 | 4,859,680 |

The candidate runs a second bracket round and consumes ~2.5M more work in the
bracket stage; the equal-fragment tail entry drops from 6,977,179 to 4,859,680,
a difference of exactly 2,117,499. The candidate produces more domains (33 vs
20 per round) but neither domain identity nor the W2 loss mechanism (domain
overlap vs starvation) is proven by these counters; no speculative guard was
implemented.

Status: candidate rejected (no gain on 6 pairs, W2 regression). Core restored
byte-exact to HEAD 1339303; patch and traces preserved here. H36/H37 evidence
checkpoint; full36 and the five gates were not run at measurement time; the restored HEAD source is byte-identical to the H32 final source, whose five gates and 48-fixture verify are reused (see h32-suffix-full-iteration-017-native/gates.txt).

Files: h37-views.patch.gz, h37-trace-w2.jsonl.gz, h37-trace-w2-capture.log.gz,
h37-baseline-trace-w2.jsonl.gz, h37-baseline-trace-w2-capture.log.gz,
h37-baseline-binding.json.gz, h37-w2-loss.json.gz/.py.gz/.log.gz,
h37-compare.json.gz, retention-irs-w2-*.json.gz/.log.gz, binding.json.gz.

Note: `h37-w2-analysis` is a truncated region-set comparison and does not prove
ownership or cause; the canonical loss evidence is `h37-w2-loss.{py,json,log}.gz`.
