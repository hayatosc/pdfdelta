# Source quotation gold

`development.json` registers source quotation units and form-field correspondences
before modifying anchor search. Schedule C source pages were rendered and inspected;
EDPB introduction quotations were checked in both rendered pages and independent
Poppler text. The remaining Schedule C candidates were known when selecting those
regions, so these are development targets, not an independent accuracy estimate.

The labels for energy deductions and other expenses follow their visible fields,
not a flattened order interleaving the left and right columns. Ambiguous shortest
character edits in the Part V range are deliberately not assigned a single position
mask. These units cannot score global event accuracy or exact glyph positions.

`verify_source_gold.py` verifies input hashes and unique page quotations without
reading engine reports. Form-field quotations retain separate label and body boxes in PDF points. The
verifier reads those boxes through independent Poppler word geometry, concatenates
the label pieces, and checks the full quotation. It does not pretend that the
field is contiguous in a flattened two-column text stream. These boxes identify
source evidence; they are not engine glyph-position gold.

Independent evaluation must use a separately frozen split, with the original PDF
as evidence and no development target or engine output used to write the labels.
Experiment logs and rendered pages belong outside the repository.

`heldout.json` contains ten separately frozen EDPB body-text scopes from sections
2–3, selected by the parent reader before the implementation experiments. That
reader did not inspect engine output for these scopes, but had read the earlier
aggregate analysis and is an agent in the same session. This is a limited
independent quotation evaluation, not an external human annotation study.
Run the same source verifier against this file to audit its twenty references.
