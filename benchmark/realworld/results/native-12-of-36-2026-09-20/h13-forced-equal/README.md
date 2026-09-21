# H13 forced-equal child promotion (accepted candidate)

Status: candidate code complete with integration regressions, mutation control,
all seven gates exit 0, and a final-binary full 36-pair capture. Acceptance is
pending the full-panel retention audit result.

## Change

- `semantic::mandatory_match_analysis` computes the unique eligible equal edge
  per rank of every maximum LCS matching with one suffix table and two rolling
  prefix rows. The full work is charged before allocation, the combined peak
  covers the suffix, rows, rank slots/flags and the fallibly reserved output,
  and an unaffordable analysis returns unavailable without erasing the shared
  remainder. Only `LimitExceeded`/`Unresolved` become an unavailable optional
  analysis; other errors propagate.
- `Assessor::forced_equal_child` applies only to the intended ambiguous whole
  multi-block domain with empty parent reasons and a completed parent search.
  It charges a conservative group/locate bound computed from the full parent
  key (proof groups plus the prefix/child rebuilds inside `locate_in_group`)
  before building anything, rejects unequal slices before any table, caches by
  `DomainKey`, honours `will_stop`, uses the child extents and separators for
  the local key, and preserves the child comparable lengths in its proof.
- The promoted relation carries `ComparisonAssumption::MandatoryMatchingEquality`
  with an empty stable-event signature; no `ExactTextDisplacement` label is
  added. `validate_semantic_emission` gains a wider overlap veto for these
  relations: any change occurrence whose projected source intervals overlap the
  claim keeps it tentative, protecting partial crossings and moves that the
  contained comparison and the move skip never saw.

## Integration regressions

- `mandatory_matching_equality_establishes_fixed_equal_children` (programmatic
  `compare_aligned`, old ["Q","a a","Q"] vs new ["Q","a","Q"]): both boundary Q
  children are Established with the new assumption while the middle deletion
  stays tentative with `AmbiguousEditLocation`.
- `repeated_equal_text_without_mandatory_positions_is_not_promoted`: three Q
  blocks against two keep an equal Q child tentative with no new assumption.
- `missing_parent_premises_block_mandatory_matching_promotion`: the parent
  carries `NormalizationUncertainty` and no forced-equal promotion happens.
- `forced_equal_claim_is_vetoed_by_crossing_change_and_move`: a claim over
  tokens 1..3 against a crossing occurrence 0..2 (not contained) and a Move
  variant both reject through the new branch; an unrelated change does not.
- `mandatory_analysis_matches_the_exhaustive_path_oracle`: every edge and word
  pair of lengths up to three over two alphabets matches the independent
  all-path intersection oracle.
- Unit coverage also checks the unaffordable analysis preserves the remainder
  and the repeated-token diagonal counts.

## Mutation control

`mutation-control.txt`: disabling only the new forced-equality overlap branch
(`if false && self.forced_equal_relations.contains(&index)`) makes the crossing
regression fail with exit 101; restoring the exact source bytes (verified by
the same SHA-256 before and after) returns exit 0.

## Gates

The seven required commands (`cargo fmt --all -- --check`, workspace clippy
with `-D warnings`, workspace tests, all-features library tests, rustdoc with
`-D warnings`, generated-fixture `pdfbench verify`, `git diff --check`) all
exited 0 in the final source state.

## Final full-panel capture

`h13-full-iteration-004-native`, final binary `fbab3082424a`, 36/36 captured,
3 complete (unchanged). Differing pairs versus the accepted H2 capture:

| pair | H2 | H13 | changes |
| --- | --- | --- | --- |
| irs-w4-english | 5,492 / 6,081, 416 unresolved | **5,725 / 6,314, 374 unresolved** | 2 -> 2 |
| nist-sha-1803 | 5,924 / 5,922, 1,018 unresolved | **5,946 / 5,944, 1,013 unresolved** | 6 -> 6 |
| all other pairs | identical metrics | identical metrics | unchanged |

`archival` note: the earlier five-pair audits and the 003 capture were
superseded by the final-binary capture; the per-pair audit files for the final
capture live in `retention-final/`.
