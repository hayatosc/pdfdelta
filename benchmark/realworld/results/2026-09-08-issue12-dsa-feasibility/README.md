# DSA-note feasibility experiment, 2026-09-08

Outcome: no detection improvement. The assumption that repairing local
correspondence alone would recover the FIPS DSA note was insufficient.
The ordinary source fixture still emits its five existing edits and does
not establish the reviewed DSA insertion. No production implementation or
annotation change remains from this experiment.

## Question and evidence

The fixed expectation `dsa-legacy-verification-note` asks for an insertion
containing the 107-character quote beginning `no longer specified` and
ending `digital signatures`. The old introduction already contains a DSA
paragraph, before RSA and ECDSA. The new introduction places a rewritten
DSA paragraph after the other algorithms. This creates competing
correspondence and ordering possibilities.

The [ordinary fixture comparison](ordinary-fixture-comparison.json) uses
the existing original-glyph reductions and the normal pipeline. It emits
five established changes and 20 candidates. The [local evidence capture](local-correspondence.json)
shows trusted introduction runs with 1,431 old and 2,509 new comparable
tokens. The DSA note is at new tokens 1611..1718.

The only closed domain immediately before the note is an equal anchor,
old 416..432 to new 1594..1610. No discovered domain covers the note. The
shared ending `digital signatures.` occurs three times in each reduced
introduction, so it cannot supply an independently unique right anchor.
The old DSA paragraph precedes RSA; the new DSA paragraph follows it.
An additional exact match connects the old title's `Federal Information
Processing` wording to the new citation after the note. These anchors do
not form a common adjacent sequence enclosing the insertion.

This confirms a correspondence blocker in the reduced case. It does not
establish that the full document has no usable additional evidence, or that
no different correspondence design could recover the change.

## Granting correspondence did not suffice

Two counterfactual calls to `compare_aligned` supply a single matched block
with known order, complete passage boundaries, and deliberately favorable
high-confidence scores. The first supplies only the two DSA paragraphs;
the second supplies the two complete captured introduction runs. These are
external premises, not correspondence established by the normal pipeline.
The counterfactual does not preserve the glyph-level evidence of the ordinary
fixture and must not be counted as a successful source-backed comparison.

| Supplied correspondence | Established changes | Candidates | Candidate covering the fixed quote | Assessment work / limit |
| --- | ---: | ---: | --- | ---: |
| DSA paragraphs, 261 / 150 tokens | 0 | 12 | None | 128,538 / 32,000,000 |
| Introduction runs, 1,431 / 2,509 tokens | 0 | 74 | Insertion, new 1611..1791, relation 67 | 9,259,464 / 32,000,000 |

Both searches complete with `AmbiguousEditLocation`, not budget exhaustion.
The [structured results](counterfactual-results.json),
[paragraph output](assumed-paragraph-comparison.txt), and
[introduction output](assumed-introduction-comparison.txt) retain the evidence.
The [counterfactual program](assumed_passage_probe.rs.txt) is retained as an
experiment artifact and removed from the compiled example targets.

## Concrete ambiguity witnesses

The [exact LCS witnesses](boundary-witness.json) separate two problems:

1. With the DSA paragraphs supplied as counterparts, inserting ` no longer`
   at old boundary 40 or `no longer ` at boundary 41 gives the same optimal
   total edit cost, 165. The exact source boundaries differ. Treating the
   entire fixed quote as inserted costs 325 instead: paragraph correspondence
   and the fixed insertion expectation are different interpretations.
2. With complete introduction correspondence, an optimal script can insert
   the entire fixed quote at total edit cost 1,746. Equally optimal scripts
   can instead match characters within that quote to old text. Of its 107
   characters, 33 individually have an equal-token alternative on an optimal
   path, including non-whitespace characters. This is not solely a choice
   about leading or trailing spaces. The 33 alternatives need not all occur
   together in one script.

The [standalone verifier](verify-boundary-witness.py) reconstructs exact
prefix/suffix LCS tables and checks the recorded scores, insertion-boundary
witnesses, and every listed alternative. Its [run log](witness-verification.log)
records success. It uses only the saved text inputs and Python's standard
library; it does not alter the production matcher or annotations.

## Decision

Do not add a broader anchor search, increase work limits, or force this
candidate to become an established insertion on this evidence. The current
all-optimal-path uniqueness condition rejects the supplied cases even after
correspondence is granted. A successful approach would need additional
independently justified correspondence constraints or an explicitly revised
policy for representing ambiguous edit boundaries. A whitespace-only rule
would not address the introduction's non-whitespace alternatives.

The original expectation remains fixed. The preceding measured result,
1/25 focused expectations with two scoped false-positive tokens, remains
unchanged. The five PDF pairs were not rerun because all 127 production and
test source hashes and all 12 annotation-file hashes match the preceding
[validated scope follow-up](../2026-09-08-issue12-local-comparison/scope-projection-followup.md).
Its 2,067-test validation remains the applicable code validation; this
experiment adds no claim of a new workspace test pass or release readiness.
Issue #12 remains open.
