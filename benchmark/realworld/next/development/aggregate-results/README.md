# Development aggregate

Twenty-four active pairs contribute 144 baseline/current attempts across three
separate routes. Two unavailable arXiv revision attempts remain in
`replaced-attempts.json`. Each family has four active pairs; these are registration
strata, not proof that every named presentation trait was exercised.

All current captures bind executable SHA-256 `a74bf905…`. Baseline commit
`3f318d7` has 39 observations from executable `518ac203…` and 33 from its recorded
rebuild `22b90fa9…`. Complete hashes, build labels and source pointers are in
`pairs.json`. The arXiv working-tree build is the same current executable.

## Separate accuracy denominators

Annotations resolve for 23/24 pairs and 102/106 selectors. ECB's four selectors
remain unresolved after its old input exceeds the CID-width limit. Literal-source
extraction reports complete on 30/46 observed input sides, with two unobserved
sides. This differs from the earlier individual-source inspection's 47/48 captured
outputs; neither measure certifies all visible content.

There are 23 frozen positive scope targets, but only three independent strict
numeric targets: Schedule C, W-4 and BERT. Schedule SE has no unique internal
position gold. Native reports 68 events per revision; two match the numeric gold,
and the other 66 remain unscored rather than being declared false positives.

| Registered family | Native numeric events, baseline → current | Shared B predictions, baseline → current | Shared frozen scope hits | Current B posthoc visible-content judgments |
| --- | --- | --- | --- | --- |
| ID-free prose | undefined | 0 → 0 | 0/4 | no predictions |
| Heading/move/list/copy | undefined | 0 → 0 | 0/4 | no predictions |
| Table/form | 2/2 → 2/2 | 0 → 0 | 0/4 | no predictions |
| Multicolumn | 0/1 → 0/1 | 0 → 39 | 0/4 | 39/39 |
| Japanese/vertical/rotated/ruby | undefined | 0 → 0 | 0/4 | no predictions |
| Clip/overpaint/figure/scan/mixed | undefined | 0 → 8 | 0/3, plus 1 unresolved | 6/8 |

Shared text and all-channel routes repeat the same B predictions; their separate
rows in `families.json` are not independent samples. Shared strict numeric recall
is 0/3 events and 0/6 source atoms per route. Native restricted recall is 2/3 events
and 4/6 atoms; the two detected numeric events and their four atoms have restricted
precision 1.0. Zero-detection precision is undefined; whole-document strict
precision remains unknown. Native legacy proven regions are not shared B.

All 47 new B units have prior posthoc source-range adjudication. The 45 visible
changes comprise 38 bibliographic intervals, one ID-free Berkeley funding paragraph
and six NIST body units. Two NIST units change only retained whitespace or glyph
mappings. These results demonstrate recovery from independent producers, but do
not enlarge the frozen recall denominator. DDPM recovery occurs in single-column
regions despite its family's name. Separate column and page-break controls are
in `../../layout-controls/`.

Scope C is zero in every captured shared report. All-channel comparison has four
legacy IRS page-render proposals; their visual adjudication is retained in
`../irs-results/inferred-adjudication.json`. Text comparison has none. C and
additional source references are never added to strict recall.

## Failures and stages

Each revision captures 69/72 attempts, or 138/144 total. W-4 fails both shared
routes at the descendant-ownership limit; ECB fails native acquisition. W-4's
failed numeric attempt remains a shared miss. Every captured report is incomplete:
end-to-end complete is 0/144. All failures remain in family denominators.

`../stage-evidence.json` corrects earlier optimization labels that measured only
`matching.conflict_search_complete`. Full optimization additionally requires every
matching component to be exhaustive. Seven pairs have complete conflict search
but incomplete components: GPT-3, Llama 2, care skills, NIST SHA, Schedule C,
Schedule SE and NIST SSDF. These are measurement corrections, not new regressions.

| Shared stage, per revision; 23 captured / 24 attempted | Text complete | All-channel complete |
| --- | ---: | ---: |
| Candidate enumeration | 4 | 2 |
| Conflict search | 22 | 22 |
| All optimization components | 15 | 15 |
| Text candidate search | 4 | 4 |
| Admitted channel inventories | 0 | 0 |
| Complete channel source comparison | 0 | 0 |

Stage flags concern retained candidates. Empty optimization sets do not establish
complete acquisition. Native shared-stage entries remain null. Coverage counts,
local comparisons and unresolved counterpart decisions are separately traceable.
Zero false strict intersections on unchanged controls do not prove their complete
comparison.

Twenty-four supplementary shared captures recover unavailable stage details.
All normalized contracts match the retained historical baseline/current hashes,
including every matching component. This justifies applying the corrected fields
to the original rows. Supplementary runs in `../stage-supplement-runs.json` are
excluded from the 144-attempt aggregate and its costs; native and failed W-4
captures were not repeated for this correction.

## Costs, review burden and reproduction

Summed process times are 961.99 seconds baseline and 1,003.61 seconds current.
Maximum RSS is 1,334,460 versus 1,330,512 KiB; captured reports total 14,410,165,894
versus 14,418,036,355 bytes. These single samples ran under varying concurrent load
and do not establish PDF-wide speedup. RSS is not allocator live memory. Separate
fixed performance experiments determine performance adoption.

The funding review has 94 unchanged scalars on each side within its 291-scalar
extent. They do not enter changed ownership. Aggregate excess-context gold and
human review duration were not measured. NIST and arXiv static-review records
retain source-link and preview checks, without a human review-time claim.

Rebuild from retained records without rerunning comparisons:

```sh
PYTHON_UV=0 python benchmark/realworld/next/development/aggregate.py /tmp/dev-aggregate
```

`sources.json` binds inputs, annotations, stages, scores and capture records.
`pairs.json` has one row per pair/route/revision; `families.json` retains observed
and attempted denominators. Per-batch READMEs document source and comparison
reproduction. `capture-comparisons.py --route text --route all` can reproduce only
shared observations when needed. Blind selection and evaluation remain separate.
