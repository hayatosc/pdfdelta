# Conservative bounds for joined and curved strokes

Positive-width path strokes now retain a conservative bound using the path's outward control hull, full line width, miter limit, and the outward norm of the stroke-time transform. A single straight segment retains the former cap enclosure. Joined/curved paths expand by width times miter limit, covering permitted joins and caps without reconstructing exact stroke outlines. The enclosing Form bound remains the fallback for curves inside Forms.

The interpreter retains direct `M` and ExtGState `ML`, validates cap/join styles, and retains ExtGState `SA`. State is restored through `q/Q` and cached resource selection. Device-adjusted strokes and zero-width hairlines remain unbounded; invalid values remain unresolved. Arithmetic overflow does not become an empty extent. Acquisition cache version 16/backend profile `content-stream-v12-joined-stroke-bounds-worker-v1` prevents reuse of old bounds.

Miter joins are limited relative to line width, while automatic stroke adjustment depends on rasterization. See the [Adobe-hosted graphics-state reference](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/pdfreference1.5_v6.pdf). These are geometric bounds, not recognition, visibility or complete text-inventory certificates.

## Checks

Workspace tests pass 2,513 cases (two ignored); workspace/all-target Clippy and formatting pass. Generated fixtures pass 48/48. Tests cover cubic control hulls, affine transforms, direct/cached miter state, state restoration, enclosing Forms and unresolved device adjustment. One older test required the former unknown rectangle-stroke bound; its expectation now checks the conservative joined enclosure. No independent renderer-based stroke-envelope validation is claimed by this increment.

## Natural observations

The six-pair pilot remains incomplete in every case. Schedule C owns three additional native references, the exact `23 ` whole-node reading on both sides. Controller/processor gains three two-source equality domains but loses its previous last two domains (39 and 28 references), for a net decrease of 61 per side. Its domain count changes from 32 to 33. The cause of the late losses is not yet isolated; changed work consumption is a hypothesis, not an established diagnosis. Schedule SE retains identical coverage but changes comparison details. ECB, restrictions and NIST retain identical comparison/coverage objects.

`joined-stroke-c-source-review.json` independently checks the new whole-node raw reading and ownership. `joined-stroke-controller-source-review.json` checks the 33 retained domains' raw readings and source conservation. Neither audit independently certifies boundary correspondence, paint closure or complete inventory. No new fixed-target recovery or full-comparison gain is claimed.

The frozen binary, dirty source archive, raw captures and check logs are under `benchmark/realworld/cache/source-completion/joined-stroke-v1/`; pilot and check registrations are adjacent to this file. The pilot implementation label is the reproduction commit with the full dirty source archive; final implementation is committed as `5350325`. Timing overlapped project checks and is not a CPU performance benchmark.

Next: diagnose the controller/processor domain losses under the unchanged work budget before claiming improved source residuals. Global paint-text inventory and unresolved matching remain separate obligations.
