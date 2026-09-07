# Source review of the glossary annotations

Visual comparison of the cached PDFs confirms an annotation error. In the Approved definition, both revisions retain the algorithm-or-technique lead-in. Revision 5 removes the comma before the second alternative; it does not replace that lead-in with the term label. The current expected replacement and the new glossary scope start encode interleaved table-cell extraction instead of the printed sentence. The new scope end similarly inserts the Association label into its definition.

The Association punctuation change itself is real: the period and following capital letter become a semicolon and lowercase letter. It remains a required recovery target.

`old-glossary.png` and `new-glossary.png` are renders of the checksum-verified cached PDFs. `evidence.json` records the PDF hashes and page indices. `original-expected.json` preserves the previous annotation; `corrected-expected.json` records the source-reviewed correction now applied to the corpus. Isolating row-order proof at unsupported lines makes these corrected scope anchors available in the retained engine.

Earlier scoped-token comparisons used the original annotation. Their unchanged metrics establish reproducibility under that annotation, not correctness against the printed glossary. The corrected annotation has been evaluated on both engine snapshots: the baseline cannot resolve the new start anchor, while the retained engine reports token precision 1.0 and recall 3/7. Baseline quality remains unavailable, so this is not evidence that precision is non-regressing. Association's punctuation correspondence remains unresolved.
