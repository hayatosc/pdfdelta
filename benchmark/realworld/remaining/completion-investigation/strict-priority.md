# Strict completion remains the primary objective

## Scope correction: PDF-native text

The primary completion metric was subsequently corrected to strict PDF-native
text (`--native-text-only`). Image pixels, path lettering and OCR are outside
the current scope, while native text inside figures and Form XObjects remains in
scope. [The scope-correction investigation](native-text-scope.md) measures the
registered baseline build at 0/36 (35 incomplete plus one memory-limit failure)
and the current worktree build at 0/36 (36 incomplete), and a v2 re-audit binds
each row to its panel inputs, runs record and report hashes. The proof chain
below describes the wider document-wide common-text contract and remains
historical context for it; the native predicate omits the non-text painting
obligation.

The user confirmed that practical quality may be measured separately, then
explicitly prioritized improving the existing 0/36 completion result. The fixed
panel, completion predicate and uncertainty rules remain unchanged. A practical
quality metric is secondary; no new practical-completion field is implemented.

## Required proof chain

A completed text comparison requires all three obligations together:

1. Complete text acquisition on both sides, including possible image/path text.
2. Every discovered source accounted for by supported comparison or presence.
3. Resolved comparison search, including omitted and ambiguous alternatives.

The smallest remaining native-source residual is Schedule SE: 810 old and 812
new references. Its search now resolves, but both two-page inventories remain
incomplete, with 140 retained paint operations per revision. Closing native
source residuals alone therefore cannot complete it. Conversely, a new
acquisition proof alone cannot discharge those existing native residuals.

## Source declarations as an acquisition candidate

[PDF Association's graphic-text technique](https://pdfa.org/techniques-for-accessible-pdf/graphics-representing-text-correctly-tagged/UA1_Tpdf-G2_05/)
demonstrates an `ActualText` declaration associated with graphics representing
letters. Its [replacement-text clarification](https://pdf-issues.pdfa.org/32000-2-2020/clause14.html)
describes where those declarations belong. This is a plausible source-bound
acquisition input, subject to validated scope and conflicts; finding a dictionary
entry alone does not bind it to an executed paint operation or establish full
coverage. `Alt` is a description and cannot be treated as a literal transcription.

Artifact status does not establish absence of visible text: even an artifact may
contain headers, footers or an image of text. The existing outline/Artifact
regressions retain this obligation. Likewise, OCR confidence and relative
drawing equality do not establish exact text interpretation under this contract.

## Initial full-panel screen

The frozen existing paint probe was applied to all 72 inputs. Input hashes,
commands, executable/source hashes, raw output and limits are retained under
`benchmark/realworld/cache/completion-investigation/strict-next/declarations-v1/`.
The probe source files match the current diagnostic implementation before the
worklist correction. This run does not execute a new comparison or certify text.

- 63 probes produced observations; seven exceeded the diagnostic 200-page
  limit and two exceeded the page operator limit.
- Only 15 metadata traversals completed. Forty-six stopped at reference depth,
  one at the visit budget, and one on an unresolved object reference.
- `ActualText` was observed in the old NIST incident-handling input (18 entries)
  and the new official-writing input (4,289 entries). Both scans were partial.
- No inline `ActualText` was observed in the captured page/Form programs. The
  program and reachability limits prevent a global absence claim.

The recursive metadata probe accumulates indirect-reference graph distance into
its dictionary-nesting depth. Long reachable graphs can therefore hide candidate
declarations behind an artificial traversal stop. The bounded worklist
correction targets this investigation gap while retaining the direct-nesting,
byte and visit limits. A declarations-only mode separates this question from
unrelated page-program tracing limits. Neither change supplies a production
inventory certificate.

## Worklist correction and repeated screen

The probe now queues only object references and resolves each terminal object
once. Indirect reference distance no longer consumes the direct nesting budget;
direct nesting remains limited to 32, traversal work to 100,000 visits, and
metadata/Form bytes to 8 MiB. Alias references, dictionary back-references and
unsupported Forms retain explicit handling. The new `--declarations-only` mode
does not scan page programs, so their unrelated page/operator limits no longer
prevent the metadata question from being investigated.

The first review rejected keeping resolved objects in the queue before their
bytes were charged. The final worklist holds references only. A separate test
covers an alias object containing a reference to a terminal dictionary/Form,
with both encounter orders. Nine example tests, all-target/all-feature bench
Clippy and workspace formatting pass. The source and executable are frozen in
`declarations-worklist.json`; `declarations-checks.json` binds the check logs.

All 72 inputs now produce observations. Metadata traversal finishes on 54 inputs,
up from 15; all previously finished traversals still finish. Eighteen remain
partial: 12 reach the visit limit, four the Form operator budget, and two have
unresolvable object references. These observations do not establish the absence
of declarations outside the inspected dictionary/Form scope.

| Observed `ActualText` input | Entries | Scope/status |
| --- | ---: | --- |
| NIST incident handling, old | 18 | Metadata scan finished; cover-title/name strings and spaces |
| ECB annual report, old | 708 | Partial; 707 spaces and one hyphen |
| Official writing, new | 2,547 | Partial; Japanese text and spacing declarations |

The traversal order changes which partial prefix fits the budget. The initial
official-writing scan retained 4,289 declarations and the new traversal 2,547;
both remain preserved, and neither establishes exhaustive acquisition. No
additional inline declarations appeared in inspected Form programs.

The direct acquisition hypothesis remains unproven for the original goal.
Schedule SE and Schedule C now have finished metadata scans on both revisions,
with no observed replacement declarations; their captured page programs also
had no inline declarations. The observations above therefore do not supply the
missing text interpretation for these closest cases. The three inputs with
declarations also retain independent paint/source/search obligations. No
execution binding covering an entire natural document has been established.

## Current residuals and terminal-boundary preflight

The user has no authoring documents or generation sources available and requested
continued work with the existing inputs. The frozen post-matching executable was
therefore rerun on Schedule SE to obtain a current source review bundle. Its
comparison and coverage objects match the retained post-matching report exactly.
The bundle and raw preflight observations are under
`benchmark/realworld/cache/completion-investigation/terminal-closure/`.

The older same-row footer diagnosis is not a current implementation target:
nodes 74 and 75 already have strict coverage through the earlier tagged-region
change. The current 810/812 residual references belong to native nodes
7, 48, 50, 52, 80, 83, 84, 87 and 99, with overlapping structure views of the
same sources. Inferred direct comparisons do not own those references. That
fact alone does not rule out a separately validated native interval: an interval
can compare a changed interior under independently accepted enclosing boundaries.

A proposed terminal interval for node 99 also fails its acquisition preflight.
Its own footer rectangle is paint-free, but the region from the preceding
accepted text to the footer crosses a retained horizontal paint operation on
both revisions. The old operation 921 and new operation 946 have bounds
approximately x=34.75..577.25 and y=568.999..570.999. The footer spans
x=475.383..577.947 and y=558.476..566.827, below that operation. Inspecting only
the footer rectangle would omit the intervening effect. Complete native `/K`
termination does not resolve it: current marked-content evidence records glyph
ranges and structural completion, not a complete paint interpretation. No
terminal-boundary exception was added.

This is a failure of these specific proof paths, not a proof that no future
acquisition method can work. The distinction also appears in external validation
practice: [veraPDF](https://docs.verapdf.org/validation/) limits PDF/UA validation
to machine-verifiable checks, while the
[PDF Association graphic-text technique](https://pdfa.org/techniques-for-accessible-pdf/graphics-representing-text-correctly-tagged/UA1_Tpdf-G2_05/)
separately requires checking that extractable characters match their appearance.
Neither project supplies a drop-in exhaustive transcription certificate for the
retained path effects.

**Strict completion remains 0/36.** This acquisition investigation changes the
diagnostic only and adds no production acquisition exception. The subsequent
[cardinality solver change and its full-panel replay](cardinality-search.md)
are recorded separately; they preserve every strict coverage object.
The next production acquisition change needs an actual source-bound witness
for remaining painted text or a justified proof that a particular execution has
no text-bearing effect, followed by complete source accounting. Declaring a
reached metadata node, a recognized string or an opaque equal image to be a
completed text inventory would bypass the missing obligation and is not adopted.
