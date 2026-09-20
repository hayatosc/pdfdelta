# Short source-bounded views: candidate supply and the next evidence

This round tests whether the SE/C residuals include source-bounded single-line
views that fail only because the seed window skips views shorter than
`anchor_min_tokens` (16), and isolates what the remaining residuals actually
need. The short whole-view candidate supply was implemented, measured, and
**not adopted**: it changes no SE or C measurement, and the short views that
receive candidates are blocked by the committed position/page guard. The
production tree is back at the reviewed v4 state; no production change is kept
and no 0/36 improvement is claimed.

## Hypothesis A: the seed width

`assessment/views/anchors.rs::windows` skips any view with
`tokens.len() < width`, so a complete source-bounded singleton shorter than 16
tokens cannot seed a candidate even when the committed whole-view closure would
accept it. Instrumentation over the SE and C reports confirmed the gap and its
scope:

- SE block 7 `Attachment ` (11 tokens) is an untrusted source-bounded singleton
  whose whole sequence is unique on both sides (`count=1` each). It received no
  candidate before; a whole-view candidate supply produces a unique pair.
- SE block 4 `2024` (4 tokens) is short but changed: the old sequence occurs
  twice on the old side (the year also appears in the footer) and zero times on
  the new side, so no candidate exists — correct, no false complete.
- C has three short untrusted singletons (3, 4 and 9 tokens) with unique
  whole-view counterparts on both sides.
- SE blocks 3 `(Form 1040)` and 40 `5a ` are **inside trusted runs**, not
  untrusted singletons, so the seed width is not their blocker.

The temporary supply (whole token sequence only, never shorter windows; exact
occurrence search over every view on both sides; existing role, source-issue,
page, position and closure gates unchanged) was measured with the debug
pipeline:

| Pair | v4 baseline | with short candidate supply |
| --- | --- | --- |
| SE | 17 unresolved, 3 candidates, 8 changes, 5177/5178 resolved | identical |
| C | 84 unresolved, 7 candidates, 7 changes, 5695/5710 resolved | identical |

The SE short candidate is rejected by the committed guard because the label
actually moved: block 7's first source positions differ (501.6, 719.003) vs
(501.6, 718.503), a 0.5-point shift, and block 95 shifts by 0.003. The C short
candidates reach `close_domain` with equal positions in the 3- and 4-token
cases but produce no resolved-token delta. The supply is therefore inert on the
panel and was reverted; the production tree equals the reviewed v4 state
(`views.rs` clean, no env-gated diagnostics).

## What each remaining SE residual needs (hypothesis B)

| Residual | Shape | Missing evidence |
| --- | --- | --- |
| `[7] Attachment ` (11) | short unique singleton; candidate exists | relative position against surrounding source-backed unique anchors: the label shifted 0.5 pt, and absolute first-glyph equality cannot tell a coherent layout shift from a move. A page/position tolerance alone is not adopted. |
| `[95]` footnote (64) | unique singleton; candidate exists | same as `[7]` with a 0.003 pt shift. |
| `[4] 2024/2025` (4) | short, changed value; old sequence repeated in the footer | a change-side correspondence for short changed values: surrounding/parent anchors plus position, not equality closure. |
| `[3] (Form 1040)` (11) | inside a trusted run; region evidence `exact_canonical` | why the containing run/window keeps an exact canonical match unresolved (domain closure, competition, or parent correspondence); not candidate supply. |
| `[40] 5a ` (3) | inside a trusted run | same class as `[3]`. |
| `[2] Self-Employment Tax` (19) | untrusted singleton; no pair/chain formed | its 16-token window repeats (the part title `Self-Employment Tax `), so no unique seed exists; needs surrounding/parent correspondence to disambiguate repeated labels. |
| `[50] and railroad retirement…` (32) | untrusted singleton; three anchors reach `close_domain` | the domain path runs but the region stays; needs the domain/split/assessment reason traced (not candidate supply). |
| `[39]` space (1) | whitespace-only | excluded by design; no anchor should be supplied. |
| `[48]`, `[73]`, `[74]`, `[82]`, `[4,5]` | competition / anchor interval / text similarity | separate causes; not addressed here. |

No new proof framework, solver, common `document/text_scopes` change or
panel-specific rule was added. The next implementable step for `[7]`/`[95]` is
a relative-position proof against the surrounding source-backed unique anchors
(does the singleton keep its offset to the nearest proven anchors on both
sides?), classified as low confidence; it needs its own fixtures and is not
started here.

## Evidence

- Cache: `benchmark/realworld/cache/completion-investigation/native-short-views-v1/`
  with the instrumentation logs (`se-short-view-diag.log`,
  `se-short-hit-diag.log`, `se-guard-diag.log`, `se-mismatch-diag.log`,
  `se-pair-diag.log`, `c-short-hit-diag.log`) and the measured comparisons
  (`se/c-compare-candidates.jsonl` with the supply,
  `se/c-compare-fixed.jsonl` without it).
- Baseline: v4 binary `4d9968d2…`, SE 17/3/8/5177/5178, C 84/7/7/5695/5710.
- The committed `views.rs` and `pipeline_fixture.rs` are unchanged; the
  temporary env-gated diagnostics were removed.
- `summary.json` binds every artifact and is recursively hash-verified.
