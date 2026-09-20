# Singleton local comparison: page-bounded closure (final)

v4 closes the last gap in the whole-view exception: `PositionSignature` carries
baseline and direction only, so two singletons with identical coordinates on
different pages could previously satisfy the position check. The exception now
also requires a single shared source page. The guard is a pure hardening: the
adopted Schedule SE and C numbers are unchanged from v3, every established
change is preserved, and strict completion remains **0/36**.

## The page boundary

- `PositionSignature` stores `baseline_x/y` and `direction_x/y` for the first
  source glyph of each canonical token. It has no page identity, and
  `source_bounded_block` only requires `pages.len() == 1`; it does not compare
  old and new pages. `View` did not carry the page either.
- A same-coordinate singleton moved to another page therefore passed the v3
  whole-view check. The two-page fixture confirms the existing pipeline keeps an
  obligation for that shape (2 candidates, 2 unresolved regions, coverage
  0.771), so no false completion was observed, but the exception itself had no
  page guard.

The guard added in v4:

- `View` carries `page: Option<u32>` for source-bounded views, taken from the
  block's single page.
- `close_domain`'s whole-view acceptance additionally requires both views to
  have the same page. A cross-page same-coordinate move stays unresolved; page
  correspondence beyond the same-number case stays unresolved as before.
- The signature list must have exactly one entry per canonical token; the
  side-validation layer already enforces this and `build_views` keeps a
  defensive guard that drops the exception's evidence when it does not hold.

## Resource bounds and semantics

- The position-signature scan runs only for source-bounded views, charges
  `remaining_work` for every inspected signature, and aborts discovery on
  budget exhaustion like the other charges. Each view clones its signatures
  once under that charge.
- The signature comparison in `close_domain` is charged again (one unit per
  compared signature) because `close_domain` has no earlier charge for it; the
  comparison is bounded by the view's token count and only runs for the
  single-anchor whole-view branch.
- A `PositionSignature` is the first source glyph of one canonical token, not
  the block's full glyph geometry; the documentation now says so.

## Fixtures

`crates/pdfdelta-core/tests/pipeline_fixture.rs`:

| Fixture | Shape | Result |
| --- | --- | --- |
| `whole_view_singletons_swapped_between_sides_keep_an_obligation` | same page, A/B swap positions | obligation kept (unresolved, coverage 0.7559) |
| `whole_view_singletons_moved_across_pages_keep_an_obligation` | same coordinates, A/B swap pages | obligation kept (2 candidates, 2 unresolved, coverage 0.771) |
| `whole_view_singletons_unchanged_stay_comparable` | same page, unchanged | complete, zero obligations, coverage 1.0 |
| `whole_view_singletons_with_repeated_text_stay_ambiguous` | repeated text | unresolved, coverage 0.679 |

`crates/pdfdelta-core/src/diff/assessment/views.rs` unit tests add
`source_bounded_whole_view_anchor_requires_the_same_page` next to the existing
equal-position, partial-anchor and whole-view-closure tests (26 views tests).

## Measurements (one run per pair, limits scale 1, 6G scope)

| Pair | Unresolved regions | Candidates | Established changes | Coverage old/new | Strict complete |
| --- | --- | --- | --- | --- | --- |
| SE baseline `7c824f50…` | 19 | 3 | 8 | 0.9176 / 0.9149 | false |
| SE v4 `4d9968d2…` | **17** | 3 | 8 (identical) | **0.9438 / 0.9411** | false |
| C baseline `7c824f50…` | 116 | 7 | 7 | 0.7625 / 0.7640 | false |
| C v4 `4d9968d2…` | **84** | 7 | 7 (identical) | **0.8407 / 0.8422** | false |

Resolved tokens rise by 144 per side (SE) and 530 per side (C) with unchanged
totals. Change structures are byte-identical to the baseline in both pairs.
Costs: SE 0.26 s / 29.1 MiB, C 0.45 s / 36.7 MiB. v3 measured the same values;
the page guard changes no panel measurement because the resolved singletons are
same-page.

## Evidence binding and the v3 hash inconsistency

v3's `diagnostics.json` recorded a gate hash that no longer matched its
`summary.json` because the gates were re-run without re-binding every
reference. v4 was built after the final runs and every reference is verified
recursively:

- `diagnostics.json` binds inputs, reports, `.time` files, stdout/stderr,
  commands, 6G limits, binaries (baseline, v3, v4), the working source diff and
  every gate log with hashes.
- `summary.json` binds the diagnostics, the report snapshot and the gate log.
- A recursive verifier re-hashes every `{path, sha256, bytes}` reference and
  rejects missing files, changed hashes and the same path recorded with two
  different hashes; it reports zero stale references.

The v3 cache remains immutable history; its measurements are the same as v4's
and its gate-hash inconsistency is superseded by v4.

## Quality gates

Run serially on the final tree under the 6G scope, raw stdout/stderr/exit/
command/environment in `gates/`:

- `cargo fmt --all -- --check` — 0.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — 0.
- `cargo test --workspace` — 2669 passed, 2 ignored.
- `cargo test --workspace --all-features --lib` — 1515 passed, 2 ignored.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
  --document-private-items` — 0.

## Remaining obligations

- SE: 17 unresolved regions, 3 candidates, coverage 0.9438/0.9411.
- C: 84 unresolved regions, 7 candidates, coverage 0.8407/0.8422.
- Moved, cross-page and position-uncertain singletons stay unresolved by
  design; the year block still has no local anchor.
- Panel-wide strict completion remains 0/36.

## Evidence index

- Cache: `benchmark/realworld/cache/completion-investigation/native-trusted-runs-v4/`
  (`summary.json`, `diagnostics.json`, `gates/`, `measure/`, `logs/`, `source/`,
  frozen binary `binary/pdfdelta-4d9968d2f6b8d59d`).
- Counterexample history: `native-trusted-runs-v3/logs/counterexamples-*.txt`.
- v1-v3 caches are immutable and were not modified.
