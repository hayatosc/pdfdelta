# Issue 12 enumeration head prototype

This standalone Rust crate evaluates a conservative, source-preserving correspondence rule over the six bounded CSF candidates captured in the repository. It is an experiment and is not production integration or an acceptance-policy change.

Run the focused checks from the repository root:

```sh
cargo fmt --manifest-path target/issue12-enumeration-head-prototype/Cargo.toml -- --check
cargo test --manifest-path target/issue12-enumeration-head-prototype/Cargo.toml --offline
cargo run --manifest-path target/issue12-enumeration-head-prototype/Cargo.toml --offline
```

The binary writes `report.json` next to this file. The report records the source hashes, preserved candidate text and ranges, every pair census result, actual reciprocal best and runner scores, selected and rejected edges, required probe results, and scope limitations.
