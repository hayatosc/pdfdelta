# Crash-fixed baseline

This snapshot is original source `db7e30e6c298d844a141ed56fb2d00f8cf82bd48`
plus only `domain-separator.patch`. The patch corrects propagation of a
three-block separator pattern into a wider comparison domain; it contains no
reading-order experiment. Original crash results remain in `../baseline`.
`provenance.sha256` pins the patch and release executable.

Run all 12 annotated development pairs with two concurrent comparisons and
a 900-second per-pair external ceiling, using a fresh destination:

```sh
PYTHON_UV=0 python3 benchmark/realworld/results/issue20-order-experiment/baseline-fixed/run.py /tmp/pdfdelta-issue20-baseline/target-fixed/release/pdfbench /tmp/pdfdelta-issue20-fixed-rerun
```

The unchanged manifest/annotation hashes are in `../baseline/inputs.sha256`.
Process, summary and evaluation files follow the original baseline format.
This is development evidence, not unseen annotated holdout accuracy.

## Annotated results

All 12 annotated process attempts completed: 10 successful exits, 2 internal
resource-limit outcomes (CSF and MQTT), no crashes or external timeouts.
BIS has one unmeasurable expectation because its reviewed scope anchor is
indeterminate. The other 39 expectations yield 2 exact matches. The two
matched IDs are `association-definition-punctuation` (SP800-57) and
`publication-date` (W3C). Scoped false-positive tokens total 2, both in
SP800-57. These are development results with partial scope coverage.

`annotated-summary.json` retains every expected ID and measured/unmeasurable
status. W3C lacks detailed expected-change diagnostics in the standard
summary; its matched ID was verified against the exact emitted kind and
old/new quote in `w3c-matched-id.raw.json`, with evidence in
`w3c-matched-id.json`.

| Pair | Status | Matches | Scoped FP tokens | Seconds |
| --- | --- | ---: | ---: | ---: |
| nist-fips-186-4-to-5 | ok | 0/7 | 0 | 94.496 |
| nist-sp800-57-part1-r4-to-r5 | ok | 1/8 | 2 | 539.847 |
| irs-form-1040-2024-to-2025 | ok | 0/5 | 0 | 0.364 |
| edpb-right-of-access-v1-to-final | ok | 0/4 | 0 | 179.376 |
| arxiv-attention-v6-to-v7 | ok | 0/1 | 0 | 6.484 |
| w3c-ws-policy-attach-20060927-to-20061102 | ok | 1/4 | 0 | 15.145 |
| ecma-109-ed10-to-ed11 | ok | 0/3 | 0 | 11.102 |
| oasis-csaf-v2-cs01-to-csd02 | ok | 0/3 | 0 | 543.936 |
| irs-w4-korean-2024-to-2025 | ok | 0/1 | 0 | 0.365 |
| bis-operational-risk-2011-to-2021 | ok | unmeasurable | unmeasurable | 17.909 |
| oasis-mqtt-311-to-50 | limit | 0/0 | 0 | 384.2 |
| nist-csf-v1-1-to-v2-0 | limit | 0/3 | 0 | 26.139 |

Generated acceptance remains strict 42/48 and candidate-policy 6/6, with
zero generated false-positive tokens (`generated.log`).

## Full operational denominator

All 29 manifest pairs completed within the external ceilings: 11 `ok`,
8 `unsupported`, 9 internal `limit`, and 1 `unresolved`; no crashes and no
external timeouts. None achieved complete comparison coverage. Exit code 0
can retain an expected unsupported or unresolved extraction outcome and is
not equivalent to comparison success. `full-summary.json` retains all 29
pair statuses, split labels, coverage and process timings.

The 17 unannotated pairs used `run-operational.py` with 180-second ceilings
and two concurrent processes. Repeat with a new output directory:

```sh
PYTHON_UV=0 python3 benchmark/realworld/results/issue20-order-experiment/baseline-fixed/run-operational.py /tmp/pdfdelta-issue20-baseline/target-fixed/release/pdfbench /tmp/pdfdelta-issue20-fixed-operational-rerun
```

`inputs-rechecked.log` confirms that all manifest and annotation hashes
remain identical after evaluation.
