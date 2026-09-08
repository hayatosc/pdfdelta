# Structure and claim probe

This diagnostic reproduces the objective-compatibility and bounded claim
measurements from the issue investigation. It is deliberately separate from
the production comparison path: role domains are caller-supplied, the bounded
stamp grammar operates on supplied text, and the result is marked
`diagnostic_only` and `hypothesis_only`.

Run it from the repository root with:

```sh
cargo run --quiet -p pdfdelta-bench --example structure_claim_probe \
  > benchmark/realworld/results/structure-claim-probe/summary.json
```

The example loads the three expected-revision manifests and the three
historical order-control results with `include_str!`. It also loads four
normalized DSA text fixtures with `include_bytes!`, removes exactly one
repository newline, and verifies their recorded SHA-256 digests before any
measurement.

The probe keeps two questions separate:

| Domain | Supplied policy | Result |
| --- | --- | --- |
| arXiv distribution stamp | Version and date fields form one event | Grouping improves from two fields to one event, but the derived mask equals the literal-minimal baseline (cost 7; each side has FP 0 and FN 2) |
| CSF core-functions sentence | The whole supplied sentence is a structure-first diagnostic domain | The derived supplied-role mask costs 213 versus the literal-minimal baseline 147 (expected annotation mask: 178); its annotation gap is 65 versus 91, but false positives rise to 50 |
| DSA approved-algorithm list | DSA list-item membership | One supplied deletion event, independent of the DSA topic note |
| DSA full introduction / topic note | Full supplied introduction correspondence | The new 107-scalar note has 103..107 inserted scalars; 74 positions are certainly changed and 33 are ambiguous |
| DSA paired paragraph / topic note | Supplied old/new paragraph correspondence | The same 107-scalar query has 26..27 inserted scalars, with 22 certainly changed, 8 ambiguous, and 77 certainly same |

The DSA full-introduction result is a conditional statement about the supplied
monotone domain. After 74 certainly changed positions are resolved as a child,
the same proof leaves a 33-position remainder with 29..33 additionally changed
positions. It does not assign the same source scalar to two events.

Role transformations are derived without reading the expected changed ranges.
The distribution-stamp role parses a generic `arXiv:<id>v<version> [subject]
<day> <month> <year>` grammar, supplies version and date fields, and refines
each field and each surrounding gap with scalar LCS. The CSF role locates the
supplied function identities case-insensitively, aligns the ordered old/new
member lists, and applies the same local refinement. The untouched annotations
are consulted only afterward to report per-side true positives, false
positives, false negatives, edit cost, and event-group agreement. Each role
also records the unconstrained literal-minimal mask as a baseline. For the
stamp, the supplied policy leaves that mask unchanged, so grouping alone does
not solve the annotated Attention change.

The implementation uses a bounded scalar LCS dynamic program. Forward and
reverse `u32` tables are guarded by a 64 MiB allocation limit. A second rolling
row retains the minimum and maximum number of matches inside a queried new-side
range across all literal-minimal paths. Prefix/suffix optimality identifies
positions that are certainly changed; a split test distinguishes ambiguous
positions from positions matched on every optimal path. The example verifies
the range-bound calculation against exhaustive optimal-path enumeration for
all binary strings of length 0 through 4.

Two scalar mask guards cover `Standard` → `standard` and `. F` → `; f`.
They require the changed positions to remain local and preserve the shared
space in the punctuation case. The same strings are passed through
programmatically constructed `Document<Glyph>` fixtures, one glyph per scalar,
to verify the core pipeline entry point. The report compares the actual
canonical spans emitted by the production pipeline with the mandatory masks and
checks their exact glyph-id projection; a failure is recorded in the JSON
without changing production behavior. With the restored production pipeline,
the `Standard` fixture passes, while `. F` → `; f` fails because one emitted
span includes the shared space and both changed scalars. The scalar mask guard
still passes, so this known production limitation does not become a claimed
success.

The stamp derivation has TP 7, FP 0, and FN 4, exactly the same changed mask
as the unconstrained literal baseline. Grouping improves from two fields to
one event, but that does not reduce the fixed mask gap. The CSF function-list
derivation has false positives against its partial annotation. Neither result
justifies automatic glyph/PDF role discovery, so that extension is not
implemented. Repeated supplied function identities are rejected as ambiguous.
The decision predicate also checks an empty-mask counterexample: merely
reporting fewer changed tokens than the gold mask is not an improvement.

`summary.json` is a generated, reviewable record of the source hashes,
manifests, historical accepted-event gaps, role policies, proof values, and
fixture checks. It includes `probe_source_sha256` so the measurements can be
matched to the exact example source. It must not be read as a production
accuracy result or as evidence that PDF-level role discovery is complete.
