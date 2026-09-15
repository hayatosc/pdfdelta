# Extended graphics-state stroke widths

A `gs` invocation previously cached only its font selection. Its `LW` entry was ignored, so stroke evidence and local paint bounds used the previous `w` value. A generated PDF requests widths `[2, 30, 2, 30, 30]` through direct width commands, `q/Q`, repeated use of one graphics-state resource, and a resource without a width. Before the fix it produced `[2, 2, 2, 7, 7]`.

The cache now retains optional line width alongside optional font selection. Every invocation applies both retained settings; an omitted setting preserves the caller state. Indirect finite numeric widths are resolved through the neutral parser facade. Negative and nonnumeric widths yield unresolved extraction, and zero-width hairlines retain unknown paint bounds. Failed resources are not cached as valid selections. Resource keys, parser limits and graphics-state restoration remain unchanged.

The acquisition cache is version 15 and the native backend profile is `content-stream-v11-ext-gstate-widths-worker-v1`, preventing reuse of prior width evidence. This does not add text inventory, reinterpret path marks or certify opaque effects. The change is a prerequisite for using actual graphics state in more general stroke bounds.

The `LW` graphics-state entry specifies line width, and `gs` applies the named dictionary's parameters. See the [Adobe-hosted PDF reference](https://opensource.adobe.com/dc-acrobat-sdk-docs/standards/pdfstandards/pdf/PDF32000_2008.pdf).

## Diagnosis context

`native-denial-paint-bands.json` reconstructs the six previously diagnosed old controller/processor bands from retained source glyphs and paint records. They lie in figure pages 45 and 47 (zero-based). Each has intersecting or unknown paint bounds. Closed-stroke operators with unknown bounds obstruct every band on their page; operator spelling alone does not tell whether their paths are curved. This motivates bounded joins/curves, but those require correct line width and join state before expansion.

Rasterized path text can be submitted to the same OCR provider as image text. The earlier operator census does not establish a need for a separate path recognizer. Recognition remains inferred in the current strict source-coverage contract, including confidence 100 and an independently declared complete inventory. No completion gain follows from merely connecting OCR.

## Validation and natural pilot

Workspace tests: 2,512 passed, zero failed, two ignored. Workspace/all-target Clippy, formatting and diff checks pass; generated fixtures pass 48/48. The controls cover reference-valued width, cache reuse, omitted width, graphics-state restoration, invalid width and unresolved hairlines.

All six natural comparison objects and coverage records are exactly unchanged from `row-domain-v1`, and all six remain incomplete. This is a correctness fix, not a natural completion gain. `gstate-width-v1-pilot.json` registers the new binary, dirty source archive, input hashes, commands, raw captures and previous results. `gstate-width-checks.json` registers check logs. Frozen artifacts are under `benchmark/realworld/cache/source-completion/gstate-width-v1/`. Pilot timing overlapped project checks and is not a CPU performance benchmark.

Next: retain miter-limit state and prove conservative bounds for positive-width multi-segment and curved strokes before using those bounds in local closure. Hairlines, unsupported state and overlapping effects must remain unresolved. Such localization alone will not establish global text inventory.
