# H29 internal deleted soft line break fallback

Implementation: HEAD ce92e8a plus this source patch (sha256 2e2b21e6879dc639aac6842cbb951b35532b5ce762f2842ca87faeb2093ca0d4); release
binary 00fdd2d7bab020be6d3c24c06742dc17c5e940bd109132956d3d0967989e8b0f
(`target/release/pdfdelta`). Panel sha256
c3aa4a5dd7edb3b7b4b5144ea31a9eb48645fa5ebe5a27546f3be09b0dd6e744 (frozen,
unchanged).

## Mechanism

`EqualFragmentCache::prove_internal_deleted_soft_line_break_with_certificate`
(equal_fragment.rs) certifies an internal raw gap only when it is exactly one raw
newline scalar with a singleton `LineBreak{preceding,following}` naming the
adjacent selected real glyphs and exactly one structurally validated
`SoftLineBreak` event deletes that raw range to a zero-length canonical range at
the following selected scalar; certified break offsets must agree on both sides.
The deferred tail pass (`recover_equal_fragments`) runs the original contiguous
pass first over the existing candidate list and only then a second
allocation-free pass with the allowance. Adopted fallback ranges record the new
premise `EqualFragmentInternalDeletedBreak` (report string
`equal_fragment_internal_deleted_break`). Original `prove()` behavior, order
and charges are unchanged.

Production-cache probe (certificate path, separate cumulative diagnostic budget
from the actual remaining 5,549,122): rel573/575/605 all Proven for 215,973;
the original contiguous proof stays `Held(RawCut)` for each.

## Full015 capture (new regression baseline)

- capture: `benchmark/realworld/cache/native-12-of-36-2026-09-20/h29-fallback-full-iteration-015-native`
  (pinned, completed), route native, limit_scale 1, timeout 180 s
- 36 rows captured, 0 failed, 3 complete retained (irs-schedule-se,
  faa-thunderstorms-b-to-c, bunka-kana-1946-to-1986)
- 31 reports logical-JSON-identical to full014 (streaming gunzip + sha256); 5 differ: irs-1099-misc,
  irs-schedule-c, irs-schedule-se, mext-lower-secondary-japanese-2008-to-2017,
  nist-risk-assessment-30-to-r1
- deep retention audits against full014 all pass (no source loss, no review
  items, no problems): `retention-full015-*.json.gz`. Only irs-1099-misc changes
  coverage (unresolved regions 143 -> 117, +323 resolved tokens per side); the
  other four differ only in the top-level `assessment` work counters (measured
  per-key canonical hash comparison), with every semantic section identical.
- score remains 3/36: no pair newly reaches complete.

## Audits and evidence

- `retention-full015-*.json.gz`: deep audits (before = pinned full014 capture,
  after = pinned full015 capture).
- `retention-audit-logs.txt.gz` (pilot six-pair audits) and `retention-full015-logs.txt.gz` (full015 audits): raw logs.
- `../h29-residual-proof/`: H29 probe evidence with corrected metadata.

## Evidence files

- `full015-manifest.json`: compact manifest binding all 36 rows (old/new input
  sha256, exit code, gzip report sha256, logical report sha256 and length),
  the pinned capture marker, and every prior audit artifact.
- `gates.json` + `gates/g1..g7.log.gz`: archived gate commands, actual return
  codes, timestamps and log hashes on the identical source content.
- `classification-full015.json`: streaming paired-event classification. schedule-c,
  schedule-se and nist-risk-assessment-30-to-r1 differ in exactly two scalar work
  counters (`assessment.work_used`, `assessment.work_by_stage.emission`);
  mext-lower-secondary differs structurally by one removed `assessment.review_units`
  item (relation 179; cause unresolved, retention audit passes) with all later
  event mismatches cascading from that list-length change. An earlier nested
  classifier OOM-killed on mext (classifier-only; limits unchanged) and was
  replaced by the streaming comparison.
- `retention-audit-logs.txt.gz`, `retention-full015-logs.txt.gz`: raw audit logs.

## Hash semantics

`report_sha256` and `report_file_sha256` in `runs.json` are compressed-file
hashes; `report_logical_bytes` is the decompressed length. Logical content
hashes here were recomputed by streaming gunzip + sha256. Report producers:
full013 = 6686286e, full014 = 0a019a3c, full015 = 00fdd2d7. Instrumented
historical test-executable hashes were never recorded and stay unrecorded
rather than being reassigned.

## Gates

All seven gates pass on this patch: fmt, workspace clippy (all targets, all
features, -D warnings), workspace tests, all-features library tests, rustdoc
(-D warnings, private items), `pdfbench verify` (48/48), `git diff --check`.
