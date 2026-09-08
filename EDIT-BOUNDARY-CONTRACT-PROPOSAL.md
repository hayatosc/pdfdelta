# Deterministic edit boundaries within an established comparison

Status: proposal only. No boundary policy or acceptance expectation changes
are authorized or implemented by this document.

## Problem and scope

The existing generated matrix has 42 of 48 strict passes. Six cases remain
tentative because adjacent whitespace allows more than one optimal edit
location. For example, changing `A B` to `A X B` permits an insertion of
`X ` after the existing space or ` X` before it. Both reconstruct the same
new string and have the same cost, but attribute the surviving space to
different source positions.

The current result contract requires invariant semantic source ranges.
Choosing one script would change that contract. A canonical representation
could make these cases useful without claiming to observe how an author
edited the document. It would not establish correspondence between passages,
resolve uncertain reading order, or justify moving text.

## Proposed contract

Offer an explicitly versioned boundary policy inside a domain whose source
correspondence, internal order, and boundaries are already established.
Keep the current invariant-range policy as the default during evaluation.

Under the proposed policy, group optimal edit paths only when their
differences move an edit boundary across adjacent, exactly equal whitespace
tokens. Non-whitespace duplicates and alternative source occurrences remain
ambiguous. Unmapped tokens, extraction barriers, missing source continuity,
and incomplete relevant searches prohibit acceptance.

Define the equivalence test on exact comparable tokens before semantic
presentation. For each group, compute the same ordered sequence of changed
non-whitespace source intervals on both sides. Require every path to agree
on this sequence. A difference in the identity, extent, or pairing of any
non-whitespace interval is not a whitespace-boundary equivalence.

Select a representative symmetrically:

1. Orient the pair using a stable lexicographic comparison of the two exact
   domain token sequences. Equal sequences emit no content change.
2. In that orientation, choose the lexicographically smallest ordered list
   of `(old_start, new_start, old_end, new_end)` edit coordinates among the
   eligible optimal paths.
3. Restore the caller's old/new orientation, then apply semantic grouping.

This rule defines a result independent of traversal order and makes old/new
reversal return the transposed script. A bounded implementation must prove
that all relevant optimal paths meet the equivalence condition; reaching a
work or memory limit keeps the result tentative. Enumerating every path is
not required, but a witness alone is insufficient.

Preserve all whitespace content and exact source maps. Applying the edits
to the old comparable sequence must produce the new sequence exactly;
applying the inverse must recover the old sequence. The chosen source
positions describe the canonical representation, not historical authorship.

## Public result and compatibility

Report the boundary policy and its version with the assessment. Distinguish
invariant source localization from canonical whitespace localization in
programmatic and serialized evidence. Do not label canonical locations as
invariant. Existing callers and reports retain the current behavior unless
they explicitly select the new policy.

Keep the historical strict metric and its original expected source ranges.
Add a separate canonical metric with independently specified expectations.
A canonical pass does not retroactively turn a historical strict failure
into a pass. The five practical-release acceptance cases remain mandatory;
the six current strict failures remain visible until the release contract
is explicitly decided.

## Required evaluation before adoption

- Exhaustive short-sequence checks compare the equivalence classifier and
  canonical coordinates against all optimal paths, including mixed spaces,
  tabs, and other whitespace that survives normalization.
- Reversal, repeated words, adjacent punctuation, multiple edits, and
  repeated passage occurrences exercise the distinction between whitespace
  representation and uncertain correspondence.
- Every accepted case preserves exact reconstruction and source mappings.
  Extraction barriers and exhausted budgets remain tentative.
- Run the unchanged 48-cell strict matrix and the separate canonical matrix.
  Retain both reports, including any disagreement about semantic grouping.
- Freeze implementation and options before evaluation on independently
  reviewed local scopes. Compare exact recall, changed-token precision,
  false positives per unchanged token, coverage, and resource outcomes.

Adoption requires an explicit decision about the public localization
contract and compatibility behavior. Local correspondence recovery can
continue under the existing policy while this proposal is reviewed.

## Separate semantic presentation issue

The SP800 development capture also reports `. F` to `; f` as one semantic
replacement, including the unchanged interior space on each side. The fixed
changed-token metric counts those two spaces as false positives. Selecting
a canonical optimal path alone would not correct this envelope behavior.

A separate presentation change could expose disjoint changed ranges, but
it would need its own invariance proof. One retained atomic script does not
establish unique atomic source positions when only the semantic envelope
has been proved invariant. Keep the existing token metric and this failure
visible when evaluating either proposal.
