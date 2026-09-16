# Rendering observation experiment

Decision: retain the bounded benchmark experiment and defer a production observer
and strict visual-source binding. The installed `hayro` 0.7.1 publicly renders to
a pixmap but does not accept a caller-provided `Device`; its `Renderer` is private.
`hayro-interpret` 0.7.0 does expose `Device` and `interpret_page`. An independent
observation pass is possible without copying the renderer or modifying dependencies.

The experiment lives in `crates/pdfdelta-bench/examples/render_observer.rs` and
uses only generated fixtures. It has no external-input mode. Each PDF is bounded
to 16 KiB, one 64-by-64 page, and 1,024 retained callbacks. It adds no production
dependency or source assertion. The example's counterexamples also run in the
workspace test gate.

## Observed API and limits

| Area | Available public observation | Binding/coverage limit |
| --- | --- | --- |
| Text | Unicode mapping, outline font cache key/glyph ID, glyph and page transforms, draw mode, paint | Raw character code is private; callbacks have no native glyph ID or content-operator index. Font/glyph cache identity is not source occurrence identity. Type3 traversal is not exercised here. |
| Paths | Geometry, transform, paint and fill/stroke mode | No originating object/operator or semantic identity. |
| Images | Raster dimensions, transform; raster variant exposes its stream | A stream does not identify a particular invocation. Stencil/mask content needs its own traversal. No image text recognition. |
| Clip | Clip path and fill rule, push/pop | Does not itself prove visible text or identify a native clipping operator. |
| Transparency | Blend mode, soft-mask handle, groups and opacity, paint alpha | The fixture exercises alpha and a transparency group, not nested soft-mask rendering. |
| Marked content | Tag bytes, inline MCID, begin/end | MCID is scoped to its content context. No Form invocation ID accompanies the callback. |
| Form invocation | Nested callbacks, transformed/clipped drawing and optional transparency group | No public begin/end Form callback or invocation provenance. A group boundary is not a Form ID. |
| Coordinates | Composed affine transforms | Geometric equality/proximity does not identify a glyph. |
| Display state | Interpreter catalog optional-content state and annotation setting | Invisible optional content can be skipped. The experiment uses defaults and does not establish coverage of every alternate display state. |

Evidence comes from the pinned crate sources: `hayro/src/lib.rs::render`, private
`renderer::Renderer`, `hayro-interpret/src/device.rs::Device`,
`font/mod.rs::{Glyph, OutlineGlyph}`, `types.rs::{Paint, RasterImage}`,
`interpret/mod.rs::{InterpreterSettings, interpret_page}`, and
`x_object.rs::draw_form_xobject`. Source file hashes are retained in the result
manifest so the API claims can be checked against the measured versions.

## Experiments

The mixed fixture exercises text, a path, a clip, an image, alpha/blend state, a
transparency Form group, MCID 7, and affine transforms. It invokes the same Form
twice at the same transform. Both glyph callbacks have identical retained
Unicode, font/glyph cache identity, transforms, paint and draw mode. Callback order
distinguishes two observations but does not provide the corresponding native
invocation paths. No binding is promoted from these observations.

Rendering before and after an independent observation pass has byte-identical
RGBA samples and identical warning lists. This is a side-pass noninterference
check, **not** a pass-through observer attached to hayro's private renderer.
The latter remains untested and is a condition of any future production adoption.
Both passes use 72 dpi, white background, the same 64-by-64 output grid, default
optional-content state, and annotations enabled. Warnings are retained even when
empty. The mixed fixture produces 38 callbacks.

Four separate fixtures add a decimal point, minus sign, subscript, or thin line
to a white page. A cheap 8-by-8 center-sample feature is identical for every pair.
Detailed comparison of the complete same-grid region finds respectively 1, 8, 5,
and 8 changed pixels and records their exclusive pixel bounds. Each costs 4,096
pixel comparisons; the feature costs 64 samples. Thus a feature bucket can find
candidates, but equality cannot discard one as unchanged. Background dominance
does not prove invariant content. The fixture intentionally makes a collision;
it does not estimate real-document feature quality.

The detailed bounds are pixel changes conditional on a selected same-grid pair,
not native source ownership or recognized text. No position correction or
resampling is used to assert invariance. The production common visual supplier
continues to retain image-only regions and perform bounded same-grid RGB
comparisons as inferred results. These experiments do not replace that supplier.

## Reproduction and follow-up

```bash
cargo run --release -p pdfdelta-bench --example render_observer > /tmp/render-observer.json
cargo test -p pdfdelta-bench --example render_observer
```

`p6-results/observer.json` contains input/render hashes, profile, warnings, callback
observations, and counterexample measurements. `p6-results/manifest.json` binds
the executable and checked source files and records a single process cost sample.
It is not an end-to-end real-PDF speed or review-time comparison.

Future adoption requires an upstream/public forwarding render hook, equality of
pixels and warnings with that hook enabled/disabled, native object/operator/Form
invocation provenance, and explicit nested Type3/pattern/soft-mask/display-state
coverage. A cheap retrieval feature additionally needs a measured cost/completion
benefit while preserving detailed comparison for colliding features. Neither
coordinate matching nor a low-resolution hash can substitute for those conditions.
