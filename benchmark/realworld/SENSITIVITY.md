# Development sensitivity matrix

Run the fixed built-in corpus with layout, matching, score-margin, and
candidate-budget perturbations:

```bash
benchmark/realworld/sensitivity.sh benchmark/realworld/results/sensitivity.json
```

The command evaluates every built-in case with both renderers under six
scenarios, records the options changed for each scenario, and writes a
versioned JSON artifact. The matrix is diagnostic evidence for development;
it does not select options from holdout results or change the production
defaults.
