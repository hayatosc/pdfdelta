# Evaluation artifacts

This directory records an uncommitted implementation based on
`3bebfeb77fd43d5eaf55f9554a47e3a6d421212b`. The base revision alone does not
identify the evaluated code.

- `final-run-metadata.json` records commands, options, source and executable
  hashes, and run status.
- `current-*` records shared assessment with local-domain discovery.
- `common-*` records the same assessment without local-domain discovery.
- `production-raw.json` records the preceding production implementation;
  its accepted-output definitions differ from the new assessment contract.
- `manifest.tsv` and `annotation-manifest.json` fix the corpus and annotation
  identities. The manifest uses 19 development and 10 holdout pairs.
- `implementation.patch` reproduces the final implementation from the base
  revision. `common-from-final.patch` then disables only the local-domain
  discovery invocation in a separate source copy.
- `current-source-manifest.json` and `common-source-manifest.json` identify
  the build inputs. `final-source-manifest.json` identifies the final
  implementation and matches the current build inputs.
- `final-workspace-*` contains formatting, lint, and workspace test logs.
- `final-metamorphic.json` and `final-metamorphic.log` identify the
  metamorphic tests in that final workspace run and their source snapshot.
- `current-verify-final.log`, `common-verify-final.log`, and
  `sensitivity-ordered-final.json` retain the final generated-case results.
- `failed-origin-accounting/` retains the earlier evaluator failures and
  their source identities. They are not successful accuracy observations.
- `pre-localization-budget-fix/` retains the first completed comparison
  that exposed local-view budget starvation. `development-ordered-probes/`
  records the development checks used to validate the subsequent ordering
  correction. Later runs use the exposed corpus for regression testing.
- `per-pair-comparison.md`, `per-series-comparison.md`, and
  `production-comparison.md` summarize the final observations; the JSON
  artifacts retain the detailed metrics and denominators.
- `environment.json` and `development-fips-*` retain execution constraints
  and the development-only latency investigation.

Use separate source copies and Cargo target directories for the two methods.
Run the exact commands in `final-run-metadata.json` with fresh output paths;
the benchmark refuses to overwrite existing artifacts. Fetch public inputs
with the repository's real-world fetch command, then use the saved manifest
and its per-pair limit-scale hints. No global limit override was used.

The generated matrix preserves strict author-intent metrics separately from
candidate-policy checks: 42/48 exact expectations pass, while six additional
cells retain candidates. The latter do not satisfy the release requirement
to retain existing exact expectations. All five initial acceptance cases
pass with both renderers.
