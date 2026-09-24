# Contributing to pdfdelta

Thanks for your interest in improving pdfdelta. This guide covers the
development setup and the checks every change must pass. Repository working
agreements, including architecture invariants, are in [`AGENTS.md`](AGENTS.md).

## Setup

The workspace uses stable Rust (pinned in `rust-toolchain.toml`) with Edition
2024. [mise](https://mise.jdx.dev/) installs the pinned toolchain and provides
task shortcuts:

```bash
mise install
mise run pdfdelta -- --help     # run the CLI from source
mise tasks ls --local           # list development and benchmark tasks
```

Plain `cargo` works as well; each mise task is a thin wrapper.

## Workspace layout

```text
crates/pdfdelta-core    Pure comparison library and neutral data model (no CLI concerns)
crates/pdfdelta-cli     The pdfdelta binary: argument parsing, file I/O, reports, exit codes
crates/pdfdelta-bench   pdfbench: fixture generation, mutations, and evaluation tooling
fixtures/               Vendored test inputs with provenance
benchmark/              Real-world corpus manifests and benchmark inputs
fuzz/                   Nightly-only cargo-fuzz workspace (see fuzz/README.md)
docs/                   User and design documentation
```

Diff-engine components accept programmatically constructed `Document<Glyph>`
values, so alignment and diff work does not need PDF fixtures. See
`crates/pdfdelta-core/examples/glyph_comparison.rs`.

## Quality gate

Run the full gate before every commit; CI runs the same task:

```bash
mise run ci
```

It is equivalent to:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features --lib
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
cargo run -p pdfdelta-bench -- verify
```

Clippy enforces the `all`, `pedantic`, `correctness`, and `suspicious` groups.
The intentionally allowed pedantic lints are documented in `Cargo.toml`.

Fuzzing requires nightly Rust and `cargo-fuzz`; see
[`fuzz/README.md`](fuzz/README.md).

## Build notes

Development and test builds keep file/line backtraces but omit full debug
information, and incremental compilation is disabled to limit disk usage. For a
debugger session with local variables, build with
`CARGO_PROFILE_DEV_DEBUG=2 cargo build`. To reclaim space after toolchain or
build-setting changes, run `cargo clean --profile dev`.

## Guidelines

- Keep each change small enough to review as one coherent behavior change.
- Treat every PDF as untrusted input and keep explicit resource limits at parser
  and extraction boundaries.
- Never turn unsupported or uncertain content into empty text; report it as
  unsupported or unresolved.
- Keep [`README.md`](README.md) and [`docs/limitations.md`](docs/limitations.md)
  honest about what is not implemented.
- Add a test or fixture for new behavior. Vendored fixtures need provenance and
  a license that allows redistribution.
- Keep benchmark captures, experiment logs, and investigation notes out of the
  repository (see [`benchmark/README.md`](benchmark/README.md)).

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
