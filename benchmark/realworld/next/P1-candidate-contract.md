# Candidate-preserving text indexing

Frozen before implementation at `c93bd1d` (comparison code equals `3f318d7`).

The source candidate supplier already indexes scoped identities, table-cell keys
and literal fingerprints in `document/matching/candidates.rs`. Retrieved literal
fingerprints are checked against original tokens in the caller. Reuse this path.
No new source identity, counterpart proof or normalization rule is introduced.

The supplemental supplier in `document/text_candidates.rs` selects the same
channel-filtered children. Eligible nodes are unkeyed, nonempty text with validated
normalization, of kind paragraph/header/footer/caption/list item/code/formula.
Source-only mandatory correspondences and occupied-source conflicts are excluded
using the existing solver and dependency index. Existing alternative-group and
nonexact split/merge enumeration runs first and remains unchanged.

For every remaining same-kind old/new pair not already supplied as a singleton:

- Grams have width `min(3, token_count)`, retaining multiplicity.
- Let `common` be the sum of minimum multiplicities for equal grams, and `a` and
  `b` be gram counts. Similarity weight is
  `1 + floor(2_000_000 * common / (a + b))`.
- Zero common grams still produces weight 1. No top-k, distance cutoff, or missing
  index posting establishes absence or permits dropping this pair.
- If both normalizations are exact and original token vectors are equal, use
  `LiteralContent`, weight 1 and `literal-view-equality-v1`. Otherwise retain
  `TextSimilarity` and `literal-trigram-dice-v1`. Scores are not identity evidence.
- Completed enumeration preserves original old-major/right-minor proposal order.

On group, feature, pair or token-budget exhaustion, withdraw this supplier's
entire suffix (including supplemental groups) and retain its incomplete state and
endpoint dependencies. Earlier source proposals are not withdrawn. Failed source
protection also remains incomplete. The proposal population remains explicitly
bounded; indexing alone cannot make an arbitrarily large dense universe fit.

Index implementation must represent this whole universe, including the weight-one
complement. Kind incompatibility and unequal gram keys are exact exclusions from
feature work, not exclusions of compatible correspondence candidates. No upper
bound over competing normalizations is used to prune correspondences: accumulate
the same literal feature score for every allowed interpretation-bearing view.
Direct ordered keys compare original values; any fingerprints must additionally
verify the original values. Literal equality may be ruled out by unequal gram
multisets, but an equal multiset still requires original-token equality.

Work counters conservatively charge bounded operations and can change with indexing; completion can
improve under the same budget. Small exhaustive candidate/assignment oracles must
verify proposal membership, weights, ties and unmatched choices. Budget tests must
verify withdrawal rather than accepting a partial suffix. Separately retain the
32/128/512/2048-element matrix and fixed-real-input costs/completion, including
regressions. Production adoption remains subject to those measurements.
