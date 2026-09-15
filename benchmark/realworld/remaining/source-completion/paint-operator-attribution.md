# Remaining paint is predominantly path execution

This audit joins the retained paint-extent projection with the previously frozen operator diagnostic. It performs no new PDF extraction, recognition or comparison and changes no production decision.

The two registrations agree on each input hash. Projection and diagnostic-log file hashes were verified before the join. A retained `(page, content-stream object, operator index)` is attributed only when the historical log supplies one operator spelling and at least the retained occurrence multiplicity. Nonzero stream generations, conflicting names and missing/excess observations remain unmatched. Historical diagnostic attempts can include rolled-back operations, so address attribution alone does not establish equal execution state, clipping, visibility or dependency closure.

## Observed distribution

| Operator family | Retained records |
| --- | ---: |
| Path fill/stroke (`f`, `f*`, `S`, `s`, `B`, `B*`) | 440,078 |
| XObject invocation (`Do`) | 5,442 |
| Shading (`sh`) | 610 |
| Inline image (`BI`) | 2 |
| Unmatched | 186 |
| Total | 446,318 |

All unmatched records belong to the old ECB annual document, whose earlier diagnostic acquisition hit the subsequently fixed shared CID-width accounting limit. No operator classification is guessed for those records. `Do` does not distinguish an image from a failed opaque Form invocation.

No retained finite bound has exactly zero width or height. This does not rule out alpha-zero or other semantically empty effects; the retained projection cannot decide those conditions.

Both sides of three pairs contain only attributed path paint: W-9, Schedule C and Schedule SE. Their counts are respectively 155/162, 309/309 and 140/140. In particular, an image-only acquisition provider cannot discharge all their text-inventory obligations. Paths can encode minus signs and other textual symbols, so these counts do not authorize decoration classification or blanket removal. Existing Schedule C/SE source evidence also records real stroke-width and endpoint changes.

## Consequence for implementation

Rasterization can present both embedded images and path-drawn text to the same OCR provider; the operator counts do not establish a need for a separate path recognizer. Source attribution still needs executed geometry and graphics state, symbol alternatives and provenance. Neither a nonempty native text stream nor a no-text OCR response is a completeness proof. The current recognition adapter also keeps every OCR comparison inferred, independently of declared inventory and confidence. Exact relative paint equivalence remains a separate claim from complete text acquisition. Further boundary/row candidate additions cannot remove this inventory prerequisite.

`paint-operator-attribution.json` retains per-input counts, source hashes and unmatched counts. These observations are from the registered native snapshots, not a newly improved natural-panel result. Strict completion remains unproved, and the active goal is retained.
