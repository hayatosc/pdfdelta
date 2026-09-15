# Shared CID width resources restore native acquisition

## Result

ECB annual 2023 previously failed native acquisition at the document-wide
65,536-entry CID metric limit. Multiple Type0 fonts referenced the same descendant
CID font; one descendant was requested by nine different Type0 fonts. Its width
table was expanded and retained once per top-level font even though its source
object was identical.

The decoder now shares that immutable horizontal width table within one parsed
PDF. Each Type0 font retains its own character mapping, encoding, font identity
and other metrics. Only newly retained table entries are charged to the existing
global limit. Separate descendants remain separately charged, even when their
values happen to match. Vertical overrides remain font-local and charged.

No numerical resource limit or comparison completion predicate changed. The cache
is bound to one parsed PDF and uses canonical descendant object references; it
cannot be passed to another document. Failed font loads do not publish tables.
The extraction cache version and native backend profile changed so older cached
outcomes are not silently reused.

## Natural observations

The read-only font-access probe records 27,530 distinct expanded horizontal width
entries over the successful ECB extraction. Before sharing, repeated retention
hit 65,536 entries before extraction finished. Probe counts describe observed
resources, not which individual widths every glyph uses.

| ECB annual pair | Before | After |
| --- | ---: | ---: |
| Old native glyph records | 0 | 462,245 |
| New native glyph records | 473,275 | 473,275 |
| Old strictly compared source references | 0 | 45,097 |
| New strictly compared source references | 0 | 45,085 |
| Whole-text comparison complete | false | false |

The standalone extractor now finishes without acquisition issues. That is distinct
from text inventory: uninterpreted non-text paint still prevents complete text
acquisition. The remaining strict source residual is 417,148 old / 428,190 new,
and comparison search remains unresolved. Newly emitted comparisons are ordinary
pipeline observations, not an independently adjudicated fixed-target recovery.

Two ECB captures use the same preserved executable, input hashes, text route,
arguments and budgets. Comparison objects, coverage, contract and evidence
summaries match exactly. Timing fields need not match. EDPB controller/processor,
EDPB restrictions, Schedule C, Schedule SE and NIST contingency retain identical
comparison objects and coverage to the preceding implementation. All six pairs
remain incomplete; this is not the final two-pass 36-pair evaluation.

## Checks

- Workspace: 2,500 tests passed, zero failed, two ignored.
- Workspace/all-target Clippy, formatting and whitespace checks passed.
- Generated fixtures: 48/48 passed, keeping 42 strict author-intent and six
  candidate-policy cases separate.
- The fuzzing feature compiles with the standalone font-loader entry point.
- Shared-table tests retain different character mappings over the same CID and
  width, reject cross-document sharing, and keep failed loads from populating
  the cache. Vertical metric allocations still consume the remaining budget.
- A generated PDF uses two distinct Type0 fonts with one descendant under an
  exact two-entry limit. It preserves different text, font IDs, original codes,
  positions and operator provenance. The existing distinct-font aggregate-limit
  test still rejects a four-entry allocation under a three-entry ceiling.

## Reproduction and evidence

`cid-width-pilot.json` records the current binary/source archive, input and report
hashes, explicit capture commands, previous executable failure, tests and repeated
comparison checks. Raw artifacts are preserved in
`benchmark/realworld/cache/source-completion/cid-width-v1/`.

The diagnostic uses the existing neutral parser facade:

```sh
cargo run -p pdfdelta-bench --example cid_width_probe --locked -- INPUT.pdf
```

It delegates extraction without changing its limits and adds bounded resource
observations. Its `outcome.complete` means native extraction completion only;
it is never an inventory or whole-document comparison result. The prior worker's
resource-limit response is preserved against its executable hash. No prior
standalone diagnostic executable is claimed preserved; the corresponding source
baseline and diagnostic source are retained for rebuilding.
