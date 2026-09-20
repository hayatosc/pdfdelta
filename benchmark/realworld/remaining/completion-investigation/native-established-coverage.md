# Established local equality now reaches coverage

An established, search-complete local equality without an edit script was
dropped by `recover_local`, so already compared invariant ranges stayed
unresolved in coverage. The fix accepts exactly those domains, scoped to
source-bounded singleton views. Schedule SE drops from 17 to 14 unresolved
regions and Schedule C from 84 to 78, with every established change preserved.
Strict completion remains **0/36**.

## The gap

`assessment/local.rs::recover_local` assessed each discovered local domain and
required `RelationOutcome::Established`, then `continue`d when
`proof.edits.is_empty()`. Only the edited branch extended
`ownership.accepted`, so a proven equal domain never reached coverage.

The SE report shows the shape: relation 209 is a top-level established,
search-complete relation for block 50 range 0..48 with no changes, while the
final resolution kept block 50 range 0..32 (old and new) unresolved. A local
domain for the same range existed with `edits=0` and was dropped.

The same drop affected trusted-run fragments, ordered domains and footer
domains — but those must stay veto-only evidence: accepting them would erase
partial-order, trailing-fragment and fragment-completion obligations. Five
`diff` unit tests protect that behavior.

## Fix

- `views::LocalDomain` carries `source_bounded`, set when both closing views
  were complete source-bounded single views (the untrusted singleton shape) and
  false for trusted-run, ordered, footer and exact-anchor domains.
- `recover_local` accepts a no-edit domain only when `domain.source_bounded`
  and `records[relation].search == Complete`. The changed-ownership conflict
  check, candidate-overlap protection, output-limit checks and every closure
  and assessment gate are unchanged. A domain overlapping a tentative
  candidate is skipped, so candidates keep their source ranges.
- A unit test constructs the assessor directly and asserts the accepted
  `ResolutionState::Equal` range for the domain's source block
  (`established_source_bounded_equal_domain_accepts_its_ranges`), and a
  pipeline fixture covers an accepted change beside an equal source-bounded
  prefix.

Red to green: the unit test fails before the fix (exit 101) and passes after
(`logs/red-established.log`, `logs/green-established.log`).

## Measurements (one run per pair, limits scale 1, 6G scope)

| Pair | Unresolved | Changes | Candidates | Proven regions | Resolved old/new | Coverage old/new | Complete |
| --- | --- | --- | --- | --- | --- | --- | --- |
| SE v4 | 17 | 8 | 3 | 1 | 5177 / 5178 | 0.9438 / 0.9411 | false |
| SE fixed | **14** | 8 | 3 | 1 | **5262 / 5263** | **0.9593 / 0.9566** | false |
| C v4 | 84 | 7 | 7 | 1 | 5695 / 5710 | 0.8407 / 0.8422 | false |
| C fixed | **78** | 7 | 7 | 1 | **5960 / 5975** | **0.8798 / 0.8813** | false |

Resolved tokens rise by 85 per side (SE) and 265 per side (C) with unchanged
totals. Change structures are byte-identical to v4. SE block 50 is no longer
unresolved (old 0..54 and new 0..53 are Equal); blocks 3 and 40 remain
unresolved for their own `exact_canonical`/trusted-run reasons, as intended.
Costs: SE 0.26 s / 29.1 MiB, C 0.46 s / 38.1 MiB.

## Quality gates

Run serially on the final tree under the 6G scope, raw stdout/stderr/exit/
command/environment in `gates/`:

- `cargo fmt --all -- --check` — 0.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — 0.
- `cargo test --workspace` — 2672 passed, 2 ignored.
- `cargo test --workspace --all-features --lib` — aggregated from the raw log.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
  --document-private-items` — 0.

## Remaining obligations

- SE: 14 unresolved regions (header, year, attachment, footnotes, competition
  and footer anchor-interval causes), 3 candidates, coverage 0.9593/0.9566.
- C: 78 unresolved regions, 7 candidates, coverage 0.8798/0.8813.
- Panel-wide strict completion remains 0/36; moved, cross-page and
  position-uncertain singletons and veto-only fragments keep their obligations.

## Evidence index

- Cache: `benchmark/realworld/cache/completion-investigation/native-established-coverage-v1/`
  (`summary.json`, `diagnostics.json`, `gates/`, `measure/`, `logs/`,
  `source/working.diff`, frozen binary `binary/pdfdelta-42312e68d9675080`).
- Baseline: v4 binary `4d9968d2…` and its reports, referenced in
  `diagnostics.json`; every reference recursively hash-verified.
