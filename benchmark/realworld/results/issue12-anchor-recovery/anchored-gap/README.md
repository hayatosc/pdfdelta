# Exact diff between adjacent recovered anchors

Exact recovery previously retained the unchanged parts of a paragraph but left interior changes unresolved when sentence boundaries differed. The new path uses adjacent accepted exact anchors in the same old/new block pair to bound a gap. It rejects consumed, crossing, unsupported, or oversized gaps. Equal gaps recover unchanged coverage; changed gaps use bounded Myers diff and emit Low-confidence `AnchoredGap` events.

The four bracketing source ranges are explicit evidence. They do not fabricate a near-match score. Ownership and coverage count only the gap, while optional diagnostic contexts include the anchors and shifted edits. Near-only local-fragment review rejects this distinct evidence type.

## Measured result

All 29 manifest pairs were captured with the executable identified in `source.json`. `comparison.json` compares these results with the preceding `row-barrier` stage, using the same corrected SP 800-57 annotation. No pair loses coverage, and no available changed-token precision, false-positive rate, or reviewed recall regresses. Unavailable measurements remain unavailable. The only expected-failure-list change is recovery of SP 800-57's Association punctuation/case replacement.

SP 800-57 coverage increases from 0.678232/0.665509 to 0.685016/0.671662. Reviewed recall increases from 5/8 to 6/8. Within the two reviewed complete scopes, changed-token recall increases from 3/7 to 7/7, with precision 1.0 and no false-positive tokens. Scoped event precision and recall are both 0.75. Token success does not imply complete event matching: the Approved comma deletion still fails the annotated replacement correspondence, and only 150 of 157 annotated footer occurrences are matched.

The full-context corrected annotation cannot be evaluated on the original baseline. `stable-scope-anchor-probe/` uses shorter unique boundary anchors but still cannot locate the full Approved quote on the new side. `localized-approved-probe/` additionally shortens that quote around the same reviewed comma. Its baseline event evaluation remains indeterminate, but its independent token evaluation succeeds: precision is 3/29 and false-positive rate is 182.712579 per 10,000 unchanged tokens, compared with 1.0 and 0.0 on the repaired engine. Neither probe alters the corpus annotation. The probe's current executable is the preliminary v1 build; [the later comparability capture](../sp80057-token-comparability/README.md) confirms the result with the fixed executable and records source-range equivalence.

## Reproduction and limits

Apply `code.patch` to the commit recorded in `source.json`, then build `pdfdelta-bench` in an isolated target directory. Run the command template in `run.json` for each manifest pair. The source, patch, executable, and each capture have SHA-256 identities in `source.json`. Runtime measurements can vary.

Formatting, workspace Clippy with warnings denied, and all 1,986 workspace tests passed. Local CI passed, including 48/48 generated acceptance cases and all five mandatory core cases. Review status is recorded separately in `source.json`.

Issue 12 remains incomplete. CSF's two expected replacements still fail with `reading_order_unresolved`. `csf-adjacent-anchor-evidence.json` shows why the target pairs cannot use the bounded-gap rule. `csf-enumeration-head-evidence.json` records false alternatives that share the same ordered list and labels; those signals do not justify correspondence. No semantic model, dictionary, document-specific pairing, or threshold relaxation was introduced.
