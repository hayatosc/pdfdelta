# Benchmark data

This directory holds the inputs for benchmarks that run against data outside
the test suite. The tools that consume it are documented in
[`docs/benchmarks.md`](../docs/benchmarks.md).

```text
benchmark/
├── realworld/                 Public revision-pair corpus (documents are downloaded, never committed)
│   ├── manifest.tsv           29 development/holdout pairs with URLs, sizes, and SHA-256
│   ├── native-panel.json      Frozen 36-pair native-text completion panel
│   ├── native_panel.py        Restore, audit, and capture the native panel with failure accounting
│   ├── expected/              Human-reviewed expected changes for annotated pairs
│   ├── claims-holdout.tsv     Evaluation-only holdout without annotations
│   ├── round2-holdout/        Annotated evaluation-only holdout
│   ├── PROVENANCE.md          Development/holdout split audit
│   ├── fetch.sh               Download and verify every pair
│   ├── capture.sh             Capture a new pair's provenance fields
│   ├── capture_guard.py       Lock, disk-headroom, and output-budget guards
│   ├── flow_eval.py           Evaluate the flow engine against the PyMuPDF oracle
│   ├── flow_audit.py          Sample and classify flow-only change regions
│   ├── compact_native.py      Compact diagnostics for existing native captures
│   ├── compact_capture.py     Migrate historical schema-11 native capture runs
│   ├── native-panel-requirements.txt  Python dependencies for native panel capture
│   └── local-gold/            Independent source-quotation gold and verifier
├── manifests/                 Public smoke corpora for self-comparison
└── microbench/                Case matrices and drivers for core measurement tests
```

Downloaded documents go to `realworld/cache/` and captures go to
`realworld/results/`; both are gitignored.

## Archived evidence

Dated captures, investigation notes, and diagnostic evidence recorded during
development (previously under `realworld/results/`, `next/`, `remaining/`,
`followup/`, and `source-boundaries/`) were removed from the working tree to
keep the repository small. They remain available at commit
[`9229676`](https://github.com/hayatosc/pdfdelta/tree/9229676f47594a6cc2af8d5b54fdff64e441db15/benchmark/realworld):

```bash
git show 9229676:benchmark/realworld/results/README.md
git restore --source=9229676 -- benchmark/realworld/results   # restore locally
```
