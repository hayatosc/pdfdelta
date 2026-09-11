# Source-backed comparison: implementation and evaluation

The work starts at `3f318d7fd5cb0911e78f47fe05ea4460ae54c633` with a clean
worktree. Production code is frozen at `9093cab`; subsequent commits retain
experiments, input/source records and results. Performance changes and new
correspondence evidence have separate commits. Stable Rust, Edition 2024, the
parser facade, raw source provenance, reversible graph, exact assignment,
normalization proofs, local dependencies and explicit budgets remain in place.

## Adoption decisions

| Work | Decision and evidence |
| --- | --- |
| P0 | Adopt version-2 literal-source annotations and attribution through retained stage reasons. Quotes keep raw whitespace, scalar/byte positions and shared glyph atoms. Version-1 semantics are unchanged. See `literal/`, `diagnostics-baseline/` and `ADR.md`. |
| P1 | Adopt compatible-kind/trigram indexing with the existing bounded fallback. All compatible weight-one zero-overlap candidates remain. Ninety-six small dense/indexed cases agree in membership, order, weights and exhaustive-oracle mandatory assignments. See `P1-candidate-contract.md` and `p1-results/`. |
| P2 | Keep the implemented exact-pricing experiment test-only. It reprices root and every forbidden-edge problem, including omitted ties; oracle results agree on completed proofs. Under the same work budget, 128-element dense certification completes 12/12 trials and pricing 0/12. See `P2-pricing-contract.md` and `p2-results/`. |
| P3/P4 | Adopt bounded non-owning closed source intervals and preserve known pages of recoverable Form gaps. Unassigned glyphs, source overlap, detached/branched order, clipping/crop limits, inferred parents and acquisition gaps cannot certify unsupported closure. See `P3-scope-contract.md` and `P4-page-gaps.md`. |
| P5 | Adopt the Rust CLI's opt-in static `--review DIR` export. It retains compared PDFs, evidence, page previews, exact report pointers and A/B/C distinctions without changing ownership, coverage or exits. See `P5-static-review.md` and `final-review.json`. |
| P6 | Retain the measured benchmark observer and defer production source binding. Public interpreter callbacks lack native operator/Form-invocation provenance; the renderer has no public forwarding device hook. Same-grid pixel counterexamples disprove low-resolution equality as content proof. See `P6-render-observer.md` and `p6-results/`. |

## Measured improvement and regressions

On the fixed Korean W-4 PDF pair, indexed text feature work falls from 147,788
to 81,034 charged token visits, a 45.2% reduction, with the same 36 candidate
pairs and full result contract. Fourteen paired runs across three fixed PDF pairs
preserve all non-performance results. Timing changes are small or mixed; EDPB's
slower observations remain recorded. No general PDF end-to-end speedup is claimed.

The separate 32/128/512/2,048 text matrix completes retrieval in 42/48 indexed
trials versus 36/48 dense trials. Combined retrieval/optimization completion is
24/48 for both. Indexed 2,048-node near-equal and repeated cases retain worse
time/RSS observations while still incomplete. P1 reduces deterministic feature
work; it does not remove ownership, extraction or alignment limits. Pricing's
smaller retained edge set does not compensate for its repeated full-coordinate
solver scans and forbidden-edge certification cost.

Development comparison adds 47 B units per shared route: eight in NIST SHA
guidance and 39 in the Berkeley DDPM revision. Separate source adjudication finds
45 visible-content changes: six NIST body units, one ID-free funding paragraph
and 38 bibliographic intervals. The other two NIST units differ only in retained
whitespace or glyph mapping. These are conditional retained-source differences,
not additional visible-content recall. The independent NIST and Berkeley body
recoveries establish the additional source-traceable prose result.

The 47 units are outside the frozen development scope targets and are therefore
posthoc precision evidence, not improvements to frozen recall. Shared frozen
scope hits remain zero. Development's restricted native numeric recall is 2/3
events and 4/6 source atoms; shared strict numeric recall is zero. A, B, scope C,
legacy inferred changes and native tentative edits remain separate populations.

## Evaluation populations

| Population | Fixed denominator | Observed outcome |
| --- | --- | --- |
| Development | 24 active pairs, four per family; two failed replacement attempts retained; 144 baseline/current route attempts | 138 captured; W-4 shared ownership-limit failures and ECB native acquisition failures remain. Annotation resolves for 23/24 pairs and 102/106 selectors. Overall complete is 0/144. See `development/aggregate-results/`. |
| Frozen blind | 12 active new series, two per family; six replaced attempts retained; 72 route attempts | 72 captured; 11/12 pairs and 34/36 selectors resolve. No additional B, scope C or shared legacy inferred changes; the one strict numeric target is missed on every route. Overall complete is 0/72. See `blind/results/`. |
| Independent layout/content controls | 60 authored pairs from two producers; five content states crossed with six presentation states | Shared B exactly recovers 10/48 changed paragraphs per route with no excess source context. Native numeric events remain 10/12; shared strict numeric recall is zero. See `layout-controls/`. |
| Real-source metamorphic controls | Three pairs: comparison reversal, object renaming, unrelated page insertion | Native retains all three selected numeric events and six changed atoms. All eight original event kinds/source sets per pair also survive the declared side/page mapping. Shared routes miss those numeric targets. See `layout-controls/`. |

The content/layout matrix independently changes number, negation, unit and
affiliation, and wrapping, page break, font size, columns and paint order.
Positive/negative column and cross-page cases are retained even when B misses.
Native numeric column cases succeed; both numeric paint-order cases fail on both
revisions. Shared B succeeds only on the documented plain/font/wrapped unit or
negation cases. Native cross-page B closure and cross-kind B remain unsupported.
All 189 baseline/current layout-control contracts agree after the declared
additive-review/performance-field exclusions.

Each real-document family has its own attempted/observed denominators and
strict/scope/proposal metrics in the aggregate JSON files. No exact-position gold
means undefined recall, not zero recall. Zero predictions mean undefined
precision. Native tentative edits and unannotated A records are not silently
scored as correct. The blind set contains actual vertical rotated scans, ruby,
column changes and mixed scan/text sources, with page-specific visual records.
Registration-family names alone do not certify each trait on every input.

## Source review and remaining limits

The final NIST review bundle is generated locally at
`benchmark/realworld/cache/next-final-review/index.html`. Its 75 files contain
65,924,079 bytes, including byte-exact PDFs and 69 page previews. All manifest
hashes and 58 local HTML links were checked. It retains eight B entries and
zero A or scope C; source lists and display context do not become changed masks.
The recorded regeneration takes 4.70 seconds with 180,996 KiB peak RSS. This is
artifact-generation process cost, not human review time. The earlier P5 browser
inspection covers keyboard activation, source-page links and narrow/desktop views.

Blind result costs are 349.65 seconds baseline and 368.37 seconds current across
36 processes each. Native maximum RSS is about 3.56 million KiB and the largest
native report exceeds 10 GB. These single observations are not a controlled
PDF-wide speed claim. Raw report size remains a practical cost. Internal
resource limits, uncertain normalization, incomplete candidate enumeration and
competing correspondences explain incomplete coverage; completing an empty
candidate population does not establish complete acquisition.

The kana old PDF is visibly nonempty despite zero native glyphs; its new PDF has
unsupported clipping. FAA scans have stored text that differs from visible words.
Neither extraction success nor a source-reference count certifies visual text
accuracy. The renderer experiment does not add OCR, semantic models, a custom
PDF parser, or a geometric shortcut to strict source identity.

The blind outcome is a generalization limit, not evidence of broad prose recovery.
No production tuning follows its disclosure, so it remains a blind record. The
finite unchanged-source checks detect no false strict intersections; precision
outside the annotated/adjudicated regions remains unknown. Complete-document
success, all-PDF support and product-wide completion are not claimed.

## Reproduce and validate

Input URLs, exact hashes, failures and annotation contracts are retained under
`development/` and `blind/`; large PDFs, binaries and raw outputs stay in the
ignored cache. Use each population README's acquisition and capture commands.
The baseline must be built separately without resetting the working branch.

```sh
PYTHON_UV=0 python benchmark/realworld/next/development/aggregate.py /tmp/dev-summary
PYTHON_UV=0 python benchmark/realworld/next/blind/score.py \
  benchmark/realworld/cache/next-blind-results/baseline \
  benchmark/realworld/cache/next-blind-results/current /tmp/blind-summary
cargo run --release -p pdfdelta-bench --example render_observer
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

P1/P2 matrix runners and immutable measurement records are linked in their
individual READMEs. Static review reproduction is in `P5-static-review.md`.
Workspace checks retain all five core acceptance cases: line-wrap invariance,
page-break invariance, one replacement, one paragraph insertion and one paragraph
deletion. Generated-fixture verification passed all 48 required renderer cells
when implementation/fixture changes required it; documentation-only finalization
does not rerun that corpus. Final workspace gate results accompany the final
evaluation commit.
