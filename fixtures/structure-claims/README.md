# Source-backed structure claim fixtures

These fixed inputs support the `structure_claim_probe` example and its tests.
The four DSA text excerpts retain their original bytes; the probe validates their
SHA-256 digests before evaluating supplied-domain alignment claims. The three
JSON snapshots retain source hashes and historical accepted-event evidence used
by the probe. They are diagnostic fixtures, not production matching inputs.

Run the bounded proof checks with:

```sh
cargo run -p pdfdelta-bench --example structure_claim_probe
```

The probe distinguishes mandatory source changes from ambiguous optimal paths.
Caller-supplied domains and role interpretations do not certify automatic PDF
structure discovery. The full revision annotations remain unchanged under
`benchmark/realworld/expected`.
