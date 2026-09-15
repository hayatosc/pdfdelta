# Keep interval proof separate from optional cut discovery

## Defect and correction

After the first discovery pass, an exhausted optional-cut budget returned from
the entire range pipeline. This also skipped the new interval proof phase, even
when an admitted whole-node range review was already available and the separate
local proof budget remained untouched.

The early exit now invokes the same interval validator as the normal exit. It
does not restart discovery, replenish either budget, declare exhaustive search,
or skip native source validation. The interval phase remains bounded by its own
existing local proof-work limit.

The regression constructs the same native `ab` versus `a b` interval with
discovery budgets 500, 800, 1,000, 1,400 and 1,700. Previously each retained its
whole-node review but lost its interval certificate. All five now retain the
certificate while their cut searches remain explicitly incomplete. The pre-fix
failing test and diagnostic observations are preserved beside the new binary.

## Results

- 84 native range tests passed, including the five-budget regression.
- 2,503 workspace tests passed; zero failed, two ignored. Workspace Clippy and
  formatting checks passed. Generated fixtures passed 48/48.
- The same six-pair natural pilot has byte-equivalent parsed comparison objects
  and equal coverage compared with the preceding frozen version. ECB retains its
  seven independently checked literal interval readings and masks. The other
  five controls still produce no interval certificates.
- No additional natural recovery or newly complete pair is claimed. All six
  comparisons remain incomplete. This corrects a resource-bound control-flow
  defect; it does not resolve the outstanding inventory or search obligations.

`native-interval-budget-pilot.json` records report and executable hashes, exact
capture commands and checks. Raw reports, source snapshot, binary, driver and
test logs are under
`benchmark/realworld/cache/source-completion/native-interval-budget-v2/`.

Internal SourceCut ownership remains the next source-domain implementation step.
It must retain complete occurrence censuses, source-preserving parent cuts and
their complements, native projection validation, and unresolved rival/normalization
obligations. A serialized range review is not an ownership certificate.
