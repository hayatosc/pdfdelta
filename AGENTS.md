# AGENTS.md

## Communication

- Communicate with maintainers in Japanese.
- Write source comments, documentation, commit messages, and technical artifacts in English.
- Lead with the direct outcome and include only the evidence needed to verify it.

## Source of Truth

- Treat `SPEC.md` as the authoritative product and architecture specification.
- Implement the roadmap incrementally. Each commit should complete one coherent stage or vertical slice with its acceptance evidence.
- Do not redefine the first practical release: it requires all five cases in SPEC section 2.2.
- Keep `README.md` honest about functionality that is not implemented yet.

## Architecture Invariants

- Preserve the pipeline: raw PDF evidence -> imperfect structure -> robust alignment -> exact diff.
- Keep PDF parser-library types behind the neutral `PdfParser` and `ParsedPdf` facade.
- Preserve glyph text, raw codes, geometry, render order, render mode, and object/operator provenance.
- Keep layout reconstruction reversible so alignment can recover from incorrect line or block boundaries.
- Never turn unsupported filters, encrypted input, unmapped glyphs, or uncertain regions into empty text.
- Treat every PDF as untrusted input and enforce explicit resource limits at parser and extraction boundaries.
- Do not add a custom PDF object parser, OCR, semantic models, table recognition, or performance optimizations before the SPEC triggers are demonstrated by fixtures or benchmarks.

## Workspace Boundaries

- `pdfdelta-core` is a pure library and must not depend on CLI concerns.
- `pdfdelta-cli` owns filesystem I/O, argument parsing, report destinations, and process exit codes.
- `pdfdelta-bench` owns generated fixtures, mutations, renderers, manifests, and evaluation tooling.
- Track B components should accept programmatically constructed `Document<Glyph>` fixtures so PDF backend work does not block diff-engine work.

## Implementation Workflow

1. Locate the next roadmap stage and its acceptance condition in `SPEC.md`.
2. Trace existing definitions and callers before changing a shared contract.
3. Reuse repository or standard-library facilities before adding dependencies.
4. Implement the smallest complete behavior that advances the selected stage.
5. Add a focused fixture or test that proves the disputed contract.
6. Run the repository quality gate and review the full diff before committing.
7. Use Conventional Commits with an English subject.

Do not patch symptoms at individual call sites when the defect belongs to a shared model, parser, layout, normalization, alignment, or reporting boundary.

## Rust Standards

- Use stable Rust and Edition 2024; only a future fuzz crate may require nightly.
- Prefer safe, concrete, owned types until measurements justify a more complex ownership or allocation strategy.
- Do not introduce `unsafe`, broad shared mutation, detached tasks, or public compatibility shims without an explicit contract and direct test.
- Keep backend errors contextual while preserving the distinction between fatal, unsupported, unresolved, and resource-limit outcomes.
- Avoid fixed layout pixel thresholds; use relative metrics and tune concrete values from benchmark evidence.

## Quality Gate

Run these commands before every commit:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Also inspect `git diff --check`, scan for conflict markers, secrets, and leftover `TODO` or `FIXME` notes, and verify the behavior-specific acceptance command from `SPEC.md`.
