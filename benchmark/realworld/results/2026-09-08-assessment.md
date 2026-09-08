# Source-backed comparison assessment

These measurements describe commit `d42b27e` before integration with newer
main-branch changes. They are historical measurements, not a claim that a
full real-world evaluation has been repeated after the merge.

## Results and limits

- All five core generated acceptance cases pass: line-wrap invariance,
  page-break invariance, exact replacement, paragraph insertion, and deletion.
- The generated matrix has 42/48 strict passes. The remaining six renderer
  cells retain ambiguous candidates and pass separate candidate checks;
  these are not exact-change acceptance.
- The focused real-world gate recovers 1/25 reviewed changes: CSF 0/3,
  FIPS 0/7, ECMA 0/3, SP 800-53 1/8, and EDPB 0/4.
- SP 800-53 has two unchanged interior spaces counted as changed. The
  repaired scope projection makes all 25 expectations measurable, but
  does not itself improve recovery. Some diagnostic searches reach limits.
- The pre-merge workspace has 2,067 passing tests. Test success does not
  establish practical-release readiness or unseen-document generalization.

A DSA probe supplied trusted introduction boundaries externally and still
accepted no change. Equally optimal scalar edit scripts can insert the
entire 107-character reviewed quote or align 33 of its characters with old
text, including non-whitespace characters. Better correspondence alone is
therefore insufficient for this example under the current acceptance rule.

[Issue #20](https://github.com/hayatosc/pdfdelta/issues/20) tracks recovery,
unchanged-character false positives, and the exact-change acceptance
contract. Do not relax uncertainty safeguards or rewrite annotations just
to make the reviewed examples pass.

## Reproduction

Run the generated checks and source-backed regressions:

```sh
cargo run -p pdfdelta-bench --locked -- verify
cargo test -p pdfdelta-bench --test source_backed_local_comparison
cargo test -p pdfdelta-bench --test local_comparison_metamorphic
```

The [glyph fixtures](../../../fixtures/issue12/README.md) preserve original
source identities and provide commands for capture and comparison. The
revision manifest retains public download locations and content hashes;
normal real-world capture commands are documented in the repository README.

## Historical evidence

Large intermediate reports, temporary probes, patches, and execution logs
are excluded from the current tree. Their original contents remain available
at immutable commits:

- [Reading-order recovery acceptance](https://github.com/hayatosc/pdfdelta/blob/3bebfeb/benchmark/realworld/results/issue12-anchor-recovery/acceptance-status.md)
- [Common assessment evaluation](https://github.com/hayatosc/pdfdelta/blob/d42b27e/benchmark/realworld/results/2026-09-08-3bebfeb-evaluation.md)
- [Local-comparison execution](https://github.com/hayatosc/pdfdelta/blob/d42b27e/benchmark/realworld/results/2026-09-08-issue12-local-comparison/execution-report.md)
- [Scope-projection measurement repair](https://github.com/hayatosc/pdfdelta/blob/d42b27e/benchmark/realworld/results/2026-09-08-issue12-local-comparison/scope-projection-followup.md)
- [DSA feasibility counterexample](https://github.com/hayatosc/pdfdelta/blob/d42b27e/benchmark/realworld/results/2026-09-08-issue12-dsa-feasibility/README.md)
