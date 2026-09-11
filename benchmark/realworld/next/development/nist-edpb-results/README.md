# NIST and EDPB development references

Seven registered pairs have 30 unique literal selectors fixed before comparison.
Seventeen source pages were rendered and visually inspected. The references
cover three changed NIST abstracts, the AI RMF version-number convention,
EDPB design/default recommendations, a restrictions paragraph, and the end of
the social-targeting scope section. Each includes an unchanged content control.
These are partial finite-range references, without unique internal edit-position
gold or exhaustive whole-document precision denominators.

All 42 baseline/current process attempts capture incomplete reports with exit 3.
Both shared routes return A=0, B=0 and C=0 on all seven pairs. Frozen scope recall
is 0/7 per route; B/C precision is undefined. Native retains 12 strict events and
36 legacy proven regions for incident handling, two strict events for social
targeting, and zero strict events on the other five pairs. Every native strict
event lies outside the selected source ranges and remains unscored. Native
reports are byte-identical between revisions. No additional recovery is claimed.

## Page-break controls

The restrictions paragraph is entirely on old zero-based page 3, but spans new
pages 4 and 5. Its wording changes, including removal of "mainly" and changes to
the sentence about rights and obligations. The new continuation starts with
"such, restrictions". The source pieces exclude the intervening footnotes and
page furniture. This positive changed-range target is missed on both revisions.

The social-targeting sentence beginning "In other words" spans old pages 2 and 3
and lies entirely on new page 4. Its visible wording is unchanged, despite missing
raw space glyphs and the page break. No strict changed mask intersects any of its
annotated sources. The result is zero observed false changes, not proof that the
entire sentence was compared: no per-reference complete correspondence is claimed.

The other unchanged controls similarly have zero false strict source
intersections. Their source-atom denominators are retained in `scores.json`.
These negative observations cannot turn globally incomplete comparisons into
successful invariance proofs. The references explicitly distinguish visible
wording, literal source whitespace, line numbers, and the design/default draft's
diagonal watermark. They provide single-column and cross-page observations,
not evidence of a column-count change.

## Stages and costs

All 14 literal-source extractions report complete and all 30 selectors resolve
uniquely. This does not make shared-channel inventories complete. Text candidate
enumeration remains incomplete on 7/7 pairs; conflict optimization completes on
6/7, failing to complete for incident handling. Per-scope flags, compared-pair
counts, channel source coverage and unresolved reasons remain in `scores.json`.
No complete whole-document comparison is observed.

Shared result contracts match after excluding only wall time, additive empty
review fields and search-work counters. Exact normalized hashes and the original
native hashes are retained. Raw native reports range from 290 MB to 1.49 GB and
remain outside version control; the selected native fields preserve original
strict source locations and legacy regions. The run records retain every process
time, peak RSS, output size, input hash and executable hash. Samples ran alongside
the other revision and source inspection; they are not controlled PDF-wide
speedup measurements. Peak RSS is not allocator live memory.

## Reproduction

Build the recorded revision, verify the registered PDF hashes, and use a fresh
destination for each executable:

```sh
PYTHON_UV=0 python benchmark/realworld/next/development/capture-comparisons.py \
  target/release/pdfdelta benchmark/realworld/cache/next-dev /tmp/nist-edpb-replay \
  --pair nist-incident-handling-r2-to-r3 --pair nist-ssdf-draft-to-final \
  --pair nist-authentication-63b-to-63b4 --pair nist-ai-rmf-draft2-to-v1 \
  --pair edpb-restrictions-v1-to-final --pair edpb-design-default-v1-to-v2 \
  --pair edpb-social-targeting-v1-to-v2 --implementation YOUR_BUILD_COMMIT
```

Resolve each literal annotation with `pdfbench validate-literal-selectors` and
its registered inputs before scoring. Select native `summary`, `changes` and
`proven_changed_regions`; intersect each occurrence's side-qualified glyph IDs
with the reference source atoms. The retained intersections are all zero. For
shared reports, zero A/B/C counts directly establish missed positive targets,
but do not establish complete comparison of the negative controls.
