# Final-assessment diagnosis before local recovery changes

The comparison engine matches the previous frozen evaluation byte for byte
at the source-file level. This capture adds bounded benchmark diagnostics;
it does not alter comparison behavior. Commands, source and executable
hashes, input annotation hashes, and output hashes are recorded in
`diagnostic-runs.json` and `diagnostic-source-manifest.json`.

Both selected pairs completed with the same comparison work and result
counts as the preceding capture. Diagnostic joins completed for all listed
expectations. A location-overlapping candidate is evidence to inspect, not
a claim that its kind or ranges match the annotation.

| Expectation | Located source ranges | Final observation |
| --- | --- | --- |
| CSF `all-sector-scope-emphasized` | Old block 92, scalars 764..937; new block 43, scalars 0..204 | No final candidate occurrence overlaps both quotes. The only assessment records covering both are document-wide parents 0 and 1. |
| CSF `core-expanded-from-five-to-six-functions` | Old block 166, scalars 314..430; new block 98, scalars 0..138 | No final candidate occurrence overlaps both quotes. The same document-wide parents are the only covering assessment records. |
| FIPS `dsa-legacy-verification-note` | New block 137, scalars 41..148 | Insertion candidate 208 refers to tentative relation 3164. Its new span covers block 137, scalars 0..149; its old span is absent. Parent 1 covers all 2,734 old blocks and 1,809 new blocks. |

CSF parents retain `UnknownReadingOrder`, `InferredReadingOrder`, and
`DomainNotClosed`. FIPS parent 1 additionally retains
`NormalizationUncertainty`. These are reasons for the selected wide domain;
they do not prove that the annotated passage itself has an extraction or
normalization problem.

The CSF comparison uses 4,084,265 of 32,000,000 assessment work units. FIPS
uses 441,290,902 of 512,000,000. Neither failure is explained by exhausting
the shared assessment budget. FIPS spends 410,043,052 work units on local
views, but the insertion still has no established local parent.

Existing recovery-watch evidence places the CSF scope sentence in old page
4/run descriptor 14 and new page 4/descriptor 5; the functions sentence is
in old page 9/descriptor 22 and new page 7/descriptor 13. The FIPS insertion
is in new page 9/descriptor 12. These are document-local diagnostic indices,
not stable identities across revisions.

The isolated view probes are complete. The selected CSF old runs 14 and 22,
CSF new run 5, and FIPS new run 12 have complete, contiguous source intervals
and exact agreement between descriptor membership and trusted membership.
They are nevertheless excluded from trusted views because the descriptor's
role is mixed. Mixing a running header and body text does not invalidate the
known internal order of a body-only subrange. CSF new run 13 is already a
trusted homogeneous body view.

A separate FIPS glossary example demonstrates the whole-view pairing
restriction. Old view 62 maps to new view 197 through seven exact anchors;
old view 60 also maps to new view 197 through three other anchors. Their
source intervals are disjoint, but neither whole-view pair is permitted.
This evidence justifies interval-level split/merge tests; it does not prove
that the reviewed glossary edit already has anchors on both sides.

The source-backed reductions in `fixtures/issue12` preserve the selected
content and competing passages with original glyph evidence. The reduced
FIPS introduction has an insertion candidate for the DSA note and no
established changes. Its available exact sentence anchors end at old
tokens 387 and new tokens 302, before the note. The reduced CSF documents
provide no exact sentence recovery anchors to domain discovery. The full
PDF's mixed-role gate and the reduced documents' missing-anchor gate are
distinct observations; the reduction is not evidence that the mixed-role
classification itself is invariant under removing surrounding pages.

These findings rule out treating all misses as a permissive acceptance
problem. The implementation experiments retain entire trusted sequences for
occurrence searches, check roles on the projected comparison range, discover
bounded exact windows independently of sentence proposals, and close only
intervals supported by compatible neighboring anchors. One-sided proposals
also require an absent-side boundary from the established parent edit script.
Experiment outputs remain development evidence, not blind evaluation.
