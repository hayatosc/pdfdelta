# NIST SHA revision: retained scopes and visible content

The abstract reference was frozen before either comparison at reference SHA-256
`6b8a33d4f7ad645753d2fc226271bc2d746338aa8534d09af12b85eec6a0521b`.
Its seven literal selectors all resolve uniquely. Both rendered abstract pages
show the removal of the word “five”, with changed wrapping and unchanged heading
and keywords. The archival cover changes the PDF page index. This is one partial
scope-content target, not whole-document event or position gold.

The pre-P3 implementation `3259710` and native-closure implementation `285c794`
were compared with the same PDFs, reference and default limits. Each has three
separately recorded routes. **The frozen abstract target is detected 0/1 by both
shared routes.** Native reports exceed the 128 MiB summary ceiling, so their
annotation metrics remain unknown. All six processes exit 3.

The new implementation adds eight B review units in each shared route. All eight
were then inspected in the source renderings; these posthoc development judgments
are explicitly separate from the frozen recall target:

| Review indexes | Source observation | Units |
| --- | --- | ---: |
| 1, 3, 4, 5, 6, 7 | Visible changes to algorithm membership, timing rules or preprocessing instructions | 6 |
| 0 | Unchanged title words; only retained boundary spaces differ | 1 |
| 2 | Same visible inequalities; U+2264 versus U+F0A3 in retained mappings | 1 |

Visible body-content precision is **6/8 = 0.75 review units**, with both routes
sharing the same predictions rather than contributing independent samples. The
finite counterpart ranges were verified for 8/8 units. All eight literal source
representations differ, but the two formatting/mapping observations must not be
counted as successful recovery of visible body changes. This is an outstanding
normalization/reporting limitation, not evidence of incorrect scope counterparts.
It prevents treating the current review convention as fully validated for visible
content. Strict typed changes remain zero and strict precision is undefined.

The six body units demonstrate additional non-owning source-linked recovery on
this NIST publication, including unkeyed prose. They do not establish recall for
the unannotated remainder, independent-publisher generalization, cross-column or
cross-page closure. Review 6 also contains an unchanged second paragraph; its extra
context amount and user review time have not been measured and remain null.

`posthoc-review-adjudication.json` binds all eight report pointers, source-derived
page boxes, conditional-mask counts and rendered-page hashes. It preserves the
literal text and identifies the unchanged-context and mapping cases. The source
references themselves remain in the hash-bound reports, including every interior
and boundary glyph; these are locators, not changed ownership. Removing only the
three additive P3 fields and elapsed time yields exact equality of all preexisting
shared-route JSON, as recorded in `contracts.json`. Coverage, accepted boundaries,
unresolved dependencies and candidate decisions therefore remain unchanged.

The source resolver reports an unsupported non-rectangular clip on old page index
8, outside the abstract. Extraction gaps elsewhere are retained in its source
record and full comparison; a local positive does not make the document complete.
The rendered abstract and eight adjudication pages use the existing bounded
renderer at 612x792, white RGB background and default display state; their warning
flags are recorded. These checks do not replace the observer experiment.

Reproduce source resolution using `pdfbench validate-literal-selectors` with the
committed `annotations/nist-sha-1803-to-1804.json` and frozen cached PDFs. Reproduce
each implementation's three comparison routes with `capture-comparisons.py`, pair
`nist-sha-1803-to-1804`, a rebuilt release executable and a fresh output directory.
Run records preserve executable/input/report hashes, time, RSS and output size.
The two captures overlapped in execution, so their single observations do not
support a speed comparison. No human review-duration measurement was performed.
