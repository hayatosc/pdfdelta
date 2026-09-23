# H31: geometric prerequisite proof for the six IRS 1099 tail candidates

Base HEAD 0329bba, production source untouched (hook restored byte-exact,
`git diff` empty). Read-only budget-neutral probe: no charging calls, no proof
state mutation; rc=0 and `unresolved=117` reproduced. All 303 JSONL lines
validated. Test executable sha256 4cb9b8a9...daed8b9; artifacts hashed in
hashes.json.

## Production reference scope (corrected per review)

References are classified against the existing production scope, not against
"every Established record":

* production reference = whole original block member, complete established
  proof (Established outcome, Complete search, empty reasons), and owned on
  both sides, matching `collect_established_blocks` / `discover_translations`;
  stationary (zero-delta) and nonuniform-delta whole-block records are included
  and reported, the latter only as references that cannot support a move;
* a candidate's own exact source-span counterpart (`self_counterparts` = 1 per
  candidate) and the pending/partial equal-fragment relatives are excluded from
  the obstacle set and reported separately;
* partial references (any non-whole-block or not-fully-owned span, including
  the deferred siblings 535..547, 716/762/770/774/776/778) are never used to
  claim an all-reference failure.

Result for all six candidates: 10 production references each, with 0 source
order flips, 0 production intersects, 0 same-column-band geometry changes and
0 other-column geometry changes. The earlier 41 "crossings" were partial
pending matches in the left column (ref x <= 293.783 vs candidate x >= 324.002)
whose only flag change is vertical; the production contract does not treat
them as obstacles. Two whole-block records (relations 789, 791) fail
`finite_translation` ("not one finite uniform translation") and are loud
errors, not silent skips.

## Maximal translated suffix (717 shown; same object for all six)

Starting from the actual 717 correspondence old[226,1868]/new[225,1744]
(offset 124) and extending token-by-token with explicit scalar, direction-bit
and exact `new - old` delta-key equality:

* maximal contiguous run: old [1462, 2085) <-> new [1338, 1961), 623 tokens,
  constant delta bits (0.0, 7.097999999999956), literal-equal scalars,
  bit-equal directions, forward extension reaches both block ends
  (`ends_at_block_boundary=true`, no forward failure);
* first backward-extension failure, exactly: old[226,1461] <-> new[225,1337] is
  scalar-equal but the delta key is [-149.19799999999987, 7.749000000000024],
  different from the run key, and neither token is owned. This is the precise
  outstanding boundary;
* anchor 19 qualifies via `band_plus_translated_suffix` on both sides (band
  certificate plus the 623-token translated suffix ending at the block
  boundary and a fully mapped interstitial gap, 165/165 for 717);
  anchor 21 does not qualify because anchor 19's whole block is the
  gap obstacle (`obstacle_intersects_gap`) - expected and explicit. Band alone
  was not used to qualify the partial candidate;
* uniqueness within each candidate span: 0 transformed-key conflicts and 0
  unverifiable occurrences forward and reverse (mapped 52..176 tokens);
  `other_page` occurrences are excluded by proven page.

Intervening unselected segments are fully classified inside the candidate
blocks: for 717, old [1920,2085) / new [1796,1961), 165 tokens each, all
unresolved (accepted 0, changed 0), 3/8 deferred overlaps, and all 165 map
under the same key with scalar equality.

## H31b source-completeness measurements (exact 623-token suffix)

Read-only probe inside the live Assessor over the exact suffix
old[226,1462,2085) <-> new[225,1338,1961), using the same invariants as
`raw_source_isomorphic` but restricted to the partial canonical ranges. No
report-derived spans: all ranges come from live `BlockText` evidence.

* canonical text of the suffix is literal-equal (623 scalars);
* raw text is literal-equal (old raw [1476,2105) / new raw [1351,1980), 629
  scalars each) and all four raw cut edges are clean source-map boundaries;
* one page (4) on both sides, no page break inside the suffix;
* line-break offsets are known, valid and relatively equal (9 breaks, offset
  124), and single page;
* the six `SoftLineBreak` events touching the suffix are identical on both
  sides (same relative canonical and raw ranges, raw 1 -> canonical 0,
  `line_break` atoms); the two `AmbiguousLineBreak` issues touching the suffix
  are identical as well;
* no real glyph is shared in either raw or canonical map on either side;
  line-break and synthetic-space endpoint references are not counted.
* `holds` is empty: every source condition measured here passes; the first
  missing proof is the partial-range extension of the existing whole-block
  `raw_source_isomorphic` (`analyze_side`/`compare_sides` are whole-block and
  private), not a missing source fact.
* position translation checked separately: constant exact delta bits
  (0.0, 7.097999999999956), bit-equal directions, 623 tokens.
* real arrays: `accepted`, `changed`, `proven` and the tentative
  `ChangeCandidate` array overlap the suffix in zero intervals. The suffix
  CONTAINS seven deferred strict-closed fragments (717, 763, 771, 775, 777,
  779 and the multi-block 782 span old[226,1932,2085)) - it would resolve them
  as one translated region, not extend them fragment-wise.
* the first prior token old[226,1461]/new[225,1337] stays exactly what was
  measured: scalar-equal to its aligned counterpart but a different,
  unowned delta key. It is NOT evidence of an edit and must never become one
  through an assumption.
* loud errors: whole-block relations 789/791 fail `finite_translation`
  (nonuniform delta) and are reported, not silently skipped.

## H31b correction

The H31b `holds=[]` result was a measurement, not an executed raw-source
isomorphism proof. `atom_kind` compared only variant tags, not the full
raw-to-canonical Glyph bijection; `cut_clean` only found any map boundary, not
ordered coverage; `events_touching` excluded zero-width events exactly at cut
edges; and `page_breaks_in(None)` silently became empty. Those full invariants
remained unverified until the range API below executed them for real. The
earlier "only the API missing" phrasing was too strong.

## H31c-e range certificate implementation (uncommitted, review pending)

Files changed for review (temporary hook and probe already removed,
`git diff` contains only these two):

* `crates/pdfdelta-core/src/diff/assessment/raw_source.rs`: private
  `range_proof_core` / `raw_source_isomorphic_range` and the public
  `raw_source_range_certificate` (sharing cannot be skipped), plus
  `RangePositionRule` (`ExactEqual`, `ExactTranslation` with explicit delta
  bits), `SharedRealGlyph` hold, the `canonical_of_raw` byproduct field on the
  existing `SideAnalysis`, and 18 focused tests;
* `crates/pdfdelta-core/src/diff/assessment/equal_fragment.rs`: exposes the
  existing sharing cache through `pub(super) SharingIndex` / `sharing_index`.

Whole-block `raw_source_isomorphic` behavior and charge order are unchanged.
Range comparison reuses `analyze_side` unchanged, normalizes raw scalar
origins (not glyph counts), validates the exact raw projection of the selected
canonical range including deleted breaks strictly inside, holds on any
touching or crossing normalization event (including a deletion at the edge),
validates touching retained `AmbiguousLineBreak` issues pairwise on their
cut-relative range, canonical target offset and inside endpoint only (outside
endpoints are never claimed), compares selected literal text, relative
line-break offsets, events, issues, endpoint topology and document-wide real
glyph uniqueness. All walks and allocations are charged and fallible;
`Exhausted` never carries a proof.

## Live IRS 1099 result

First hold (H31d, before the narrow issue validation): `Held(InvalidEventRange)`
caused only by the adjacent retained `AmbiguousLineBreak` issues old raw
[1475,1476) / new raw [1350,1351), each ending exactly at its cut.

Final live source certificate (H31e) on the exact 623-token suffix
old[226,1462,2085) <-> new[225,1338,1961): **Isomorphic**, translation
checked separately as exact constant bits (0.0, 7.097999999999956) with
bit-equal directions. Work cost 207,781 units charged against a copy of the
shared budget; the live document budget and `unresolved=117` are unchanged and
no production state was mutated. Test executable bbbd221d...51c11e, JSONL six
valid lines, archived gzipped with the hook diff.

Note: the live run executable bbbd221d...51c11e was built before a no-op
code motion that moved the range API after the existing whole-block tests
module (pure addition diff, no semantic change); focused tests and clippy were
rerun on the final tree. This remains a source certificate only: it claims nothing about the outside
preceding glyph of the touching issue, no geometry/anchor integration, no
latest-ownership adoption, and no global sharing beyond the certificate's own
check. `raw_source_isomorphic_range` keeps `#[cfg_attr(not(test),
allow(dead_code))]` annotations until adoption wiring lands; remove them in the
final integrated commit.

## Decision

* The exact suffix now has a complete source certificate as measured; the
  remaining prerequisite before any adoption is integration: a charged,
  deterministic tail step that proposes this suffix as one anchored
  translated domain, runs the unchanged ownership/candidate/proven vetoes and
  keeps fragment-level candidates unadopted.
* If a smaller scope is preferred, the certificate can run per deferred
  fragment once the fragment's own cut-adjacent issue and sharing checks hold;
  that is a separate reviewable increment.
* No production adoption, commit, or full gate run is included in this
  increment; root reviews the range API and the live certificate first.

## Correction: scope of the band plus translated suffix evidence

The `band_plus_translated_suffix` observations here validated the band and
suffix geometry for the measured range only. They did not validate
own-prefix obstacles or full 623-token uniqueness. H32 adds those checks:
the adjacency obstacle scan includes the candidate block prefix with the
closed band/gap rule, and the uniqueness proof covers the whole suffix in
both directions.
