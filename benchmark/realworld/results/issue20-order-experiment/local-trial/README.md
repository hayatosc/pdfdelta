# Independently justified local-comparison trial

The trial recovers no additional reviewed changes: both versions match 2/39
measurable expectations, retain the same two matched IDs, and have 2 scoped
false-positive tokens. One expectation remains unmeasurable. The trial also
turns BIS operational-risk from `ok` into an internal `limit` outcome.

The frozen executable `/tmp/pdfdelta-issue20-local-trial/pdfbench` contains
source commit `db7e30e6c298d844a141ed56fb2d00f8cf82bd48` plus `core.patch`.
`provenance.sha256` pins both artifacts. Compare results against
`../baseline-fixed`, which contains only the common separator crash repair.

The experiment attempts to prove local changed ranges despite an ambiguous
surrounding comparison. A one-sided fragment derived from an ambiguous
parent remains withheld: a locally stable insertion/deletion boundary does
not resolve its surrounding replacement context. Existing annotations,
comparison budgets and acceptance criteria are unchanged.

`generated.log` records strict generated acceptance 42/48, separate candidate
policy acceptance 6/6, and zero generated false-positive tokens, unchanged
from the crash-fixed baseline. A policy total of 48/48 is not strict exact
acceptance.

## Reproduction and controls

Run from the repository root with a new result directory:

```sh
PYTHON_UV=0 python3 benchmark/realworld/results/issue20-order-experiment/local-trial/run.py /tmp/pdfdelta-issue20-local-trial/pdfbench /tmp/pdfdelta-issue20-local-rerun
```

The runner evaluates all 29 manifest pairs with two concurrent processes,
annotated pairs first. Annotated pairs have a 900-second external ceiling;
the 17 unannotated pairs have 180 seconds. Manifest pipeline budget hints
are unchanged. Each `.process.json` stores the command, elapsed time and
exit code or external timeout; summary and evaluation files preserve
uncertainty, unsupported input and internal resource-limit outcomes.

Timing is not an isolated performance benchmark: a CPU reading-order model
experiment runs concurrently with two workers using four CPU threads.
External timeouts, if any, remain recorded and cannot establish a semantic
regression independently of this resource contention.

The corpus has 19 development and 10 manifest-designated holdout pairs, but
all 40 reviewed changes across 12 annotated pairs are development evidence.
No genuinely unseen annotated holdout accuracy is claimed. Most annotation
scopes are partial, so scoped false positives are not full-document
precision. Input hashes are pinned in `../baseline/inputs.sha256`.

## Final comparison

All 29 pairs completed without a crash or external timeout. Baseline states
are 11 `ok`, 9 internal `limit`, 8 `unsupported`, and 1 `unresolved`; trial
states are 10 `ok`, 10 internal `limit`, 8 `unsupported`, and 1 `unresolved`.
Neither version achieves a complete comparison on any pair.

Across all 40 expected IDs, 2 matches are retained, 37 remain unmatched,
and 1 BIS expectation remains unmeasurable. Gained matches: 0. Lost
matches: 0. Retained IDs are `association-definition-punctuation` for
SP800-57 and `publication-date` for W3C. The W3C ID is independently checked
against the exact kind and quoted text in `w3c-matched-id.raw.json`; its
standard summary lacks per-expectation diagnostics.

Scoped false-positive tokens remain 2, both in SP800-57. The trial emits
3,300 accepted change events across all 29 records, compared with 2,535 in
the baseline (+765). These additional events have not been established as
correct changes by the reviewed scopes and must not be described as
improved recall or as measured false positives.

The BIS regression is an internal assessment search-budget limit, not an
external wall-clock timeout. Its reviewed scope remains indeterminate in
both runs. `comparison.json` retains all 29 state and coverage comparisons,
all 40 expected-ID outcomes, and the old/new scoped false-positive counts.

| Annotated pair | Baseline matches | Trial matches | Baseline events | Trial events |
| --- | ---: | ---: | ---: | ---: |
| nist-fips-186-4-to-5 | 0/7 | 0/7 | 317 | 461 |
| nist-sp800-57-part1-r4-to-r5 | 1/8 | 1/8 | 934 | 1299 |
| irs-form-1040-2024-to-2025 | 0/5 | 0/5 | 3 | 4 |
| edpb-right-of-access-v1-to-final | 0/4 | 0/4 | 346 | 461 |
| arxiv-attention-v6-to-v7 | 0/1 | 0/1 | 0 | 0 |
| w3c-ws-policy-attach-20060927-to-20061102 | 1/4 | 1/4 | 125 | 131 |
| ecma-109-ed10-to-ed11 | 0/3 | 0/3 | 34 | 41 |
| oasis-csaf-v2-cs01-to-csd02 | 0/3 | 0/3 | 202 | 212 |
| irs-w4-korean-2024-to-2025 | 0/1 | 0/1 | 1 | 3 |
| bis-operational-risk-2011-to-2021 | unmeasurable | unmeasurable | 70 | 119 |
| oasis-mqtt-311-to-50 | 0/0 | 0/0 | 0 | 0 |
| nist-csf-v1-1-to-v2-0 | 0/3 | 0/3 | 6 | 6 |

## Validation and artifact integrity

The parent workspace passed `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` for the trial. The latter two logs are preserved as
`clippy.log` and `workspace-tests.log`; formatting success was reported by
the parent run. These checks do not substitute for the empirical comparison.
`inputs-rechecked.log` confirms unchanged manifest and annotation hashes
after evaluation. `summarize.py` regenerates `comparison.json` from the saved
pair artifacts and matched-ID evidence.
