# Raised inline glyph source projection

A three-row native fixture reproduces a source-accounting gap: raising an unchanged inline footnote digit retains the non-owning review but formerly removes the strict changed interval. The failing expected-positive fixture was committed as `d8e751f` before the implementation.

The strict interval projection now admits an upward baseline transition only when the complete row has a common vertical bounding-box band, adjacent non-whitespace glyph bounds are disjoint from left to right, both directions are horizontal and paint order advances. Raw coordinates, readings, codes and source ownership remain unchanged. Separated rows, overlapping stacked text and whitespace baseline drift remain rejected. Existing valid projections retain their original row boundaries; the new row interpretation is used only when the original order would reject an upward transition.

The positive fixture requires one strict interval and an exact two-source replacement mask, excluding the unchanged raised digit. Negative variants separate the raised glyph vertically or overlap its horizontal bounds. The existing changed-footnote B review retains its count-only proof; this change does not upgrade that discovery result. Other source census, boundary correspondence, local paint, inventory and search requirements remain in force.

## Natural observations and rejected first revision

The first three-pair diagnostic capture is preserved in `inline-source-v1-pilot.json`. Schedule SE and Schedule C comparisons were identical to the frozen preceding panel. EDPB controller/processor retained the same compared-source counts but lost two non-owning reviews (52 to 50), with paired boundaries falling from 60 to 56. This revision was not adopted. Its frozen binary/source and raw observations remain available for inspection.

The second revision preserves the original row partition whenever the old order was already valid. Its same-input capture, `inline-source-v2-pilot.json`, still loses the same two reviews. This does not establish a successful correction. The third revision restores the original `exact_rows` helper but still loses both reviews (`inline-source-v3-pilot.json`). The source-cut row enumeration actually calls the separate `Sources::project` helper. The fourth revision retains original physical rows there too, but still loses the same reviews (`inline-source-v4-pilot.json`). A fresh capture with the preceding production binary restores 52 reviews, confirming a current-change regression rather than baseline drift. The fifth revision therefore retains the entire original discovery projection and admits mixed-baseline source order only while constructing already-bounded strict intervals (`inline-source-v5-pilot.json`). None of these diagnostics is the required final two-run 36-pair acceptance evaluation. Synthetic interval recovery must not be counted as natural target recovery or a newly complete pair.

Schedule SE discovery and native geometry observations are separately retained in `se-runs-v1.json` and `.md`. All residual nodes reach discovery; superscript order is one reproducible restriction, not proof that it explains the entire natural residual. Further work must establish the remaining boundary/closure obligations without promoting inferred matches or weakening inventory.

Check logs and provenance are recorded in `inline-source-checks.json`. Frozen caches include raw reports, executables and source archives. These comparisons use the captured source archives, which include uncommitted implementation changes relative to the reproduction commit reported by the capture driver.

## Final pilot result

The fifth revision preserves the complete comparison objects and coverage for all three pilot pairs. EDPB retains all 52 reviews and 60 paired boundaries. Compared sources remain 4,291/4,291 for Schedule SE, 5,015/5,015 for Schedule C and 2,240/2,266 for EDPB. All three remain incomplete. The strict synthetic interval improvement is established; no natural residual reduction or new completion is established by this change.

The final separation is intentional: a local source-order proof permits comparison inside an already-certified interval, while introducing mixed-baseline discovery rows requires additional boundary and population evidence. This implementation does not add those discovery rows. The first four diagnostic revisions are retained as rejected experiments, not recovery counts.

The final sixth revision additionally verifies that later glyphs retain the common vertical band of every admitted mixed-baseline row. A negative fixture narrows a later ordinary glyph so that its bounds no longer intersect the raised glyph. Both the upward and return transitions are exercised by an adjacent ordinary letter. This closes a case where checking only the transition could miss a later incompatible bound. Final checks and the repeated pilot are recorded under `inline-source-v6`.

The final sixth-revision pilot again produces comparison and coverage objects identical to the preceding production baseline for all three pairs. It therefore retains every observed B review and strict compared source in this pilot. It does not establish fixed-panel completion or target recovery. Remaining source/inventory/search obligations stay explicit.
