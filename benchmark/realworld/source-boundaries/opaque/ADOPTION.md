# Restricted opaque-effect experiment

Decision: retain `opaque-primitive-closure-v1` as a benchmark diagnostic. No W-2
page receives an equivalence certificate. The experiment changes neither text
inventory nor `comparison_complete`; historical G3 remains unmet. Its result
does not gate continued text recovery.

## Claim and supported inputs

The validator checks a sufficient condition for equal deterministic primitive
paint under one fixed profile: the entire supported page program, resolved
resources, entry state and output context must be exactly equal. It compares
the acquired structures and byte arrays directly. Hashes identify captures and
are never used instead of equality. This is relative opaque-effect equality,
not interpreted text, source ownership, author intent or document completion.

The initial profile supports rectangular paths and clips, graphics-state
save/restore, affine transforms, DeviceGray/DeviceRGB fills, explicit RGB8
image samples and finite acyclic Forms with their own explicit resources.
The whole page content program is retained, so preceding backgrounds, later
overlaps and each invocation's surrounding state remain dependencies. Forms
retain their program bytes, matrix, bounding box and nested resources. The
entry state is the PDF initial graphics state over opaque white; the footprint
is the declared media/crop intersection with its page transform.

The profile deliberately refuses fonts/text execution, marked/optional content,
annotations, AcroForm, output intents, external execution, transparency groups,
non-Normal blending and nontrivial masks. Dictionary numbers are bounded
integers: the neutral parser facade does not expose the original lexical
representation of real numbers. Nonintegral alpha therefore remains unsupported.
Decoded page commands retain their original bytes as well as parsed operands.
A regression demonstrates why: `0.100000001` and `0.100000002` collide in the
pinned content decoder's floating representation but do not certify equality.

No resource name or object number substitutes for resource contents. Missing
resources, unbalanced commands, partial samples, unsupported filters, cyclic
dependencies and exceeded limits remain unresolved. Acquisition permits at most
200 pages, 8 MiB of expanded material per page closure, 100,000 resource visits,
100,000 operations, depth 32 and a graphics-state stack of 64. Existing parser
input/object/stream limits also apply. No cache adopts a serialized certificate;
the in-process closure must be rebuilt from source. Build/profile/source and
input hashes are retained in `build.json` and the capture records.

## Dependency inventory and independent controls

| Dependency | Positive-control evidence | Independent change and result |
| --- | --- | --- |
| Commands and operands | Strictly decoded complete content; exact original command bytes | Background, overlap and caller transform/clip changes reject equality; decimal collision regression rejects equality |
| Resolved image contents | Explicit RGB8 dimensions exactly consume decoded bytes | Same `Do` commands with changed image bytes reject equality; missing samples remain unresolved |
| Entry graphics state | Declared initial state, balanced save/restore | Injected entry-state change rejects equality; unbalanced program remains unresolved |
| Transform and clip | Caller commands, Form matrix/BBox and page boxes are retained | Caller and Form transforms/clips each reject equality |
| Masks, opacity and blend | Initial state, Normal blend, no mask and integral alpha are checked | Each of nonintegral alpha, a soft mask and Multiply blend remains unresolved |
| Relevant backdrop | Declared white entry backdrop and complete preceding paint | Changed PDF background and separately injected backdrop each reject equality |
| Optional content | Catalog, page/resources and command restrictions | Visibility-bearing catalog/Form mutation remains unresolved |
| Nested invocation context | Exact caller program and recursively resolved explicit Form resources | Unchanged outer Form bytes with a changed nested resource reject equality; a resource cycle remains unresolved |
| External overlap | Entire page content retained; annotations and external paint unsupported | Added overlapping paint rejects equality |
| Output footprint | Page geometry and declared output context | Changed MediaBox rejects equality |
| Backend/profile identity | Pinned parser and profile identifiers | Each independently changed identity remains unresolved |
| Complete acquisition | Checked byte/visit/depth limits; no parser issues | Missing closure, missing resource and unsupported text state remain unresolved |

`mutations.json` contains 24 controls: one accepted positive, 18 other source-PDF
mutations and five explicitly labeled injected environment/closure controls.
All have their expected outcomes. The positive changes only PDF metadata, so
different source-file hashes still yield equal complete supported closures.
The additional lexical-decimal unit regression is recorded in the workspace
test log. Rejection means this sufficient equality proof failed; it does not
assert that two renderings necessarily differ. No raster or generic trace match
is used as a certificate.

## W-2 and retained negative cases

The registered W-2 input hashes match the earlier marker-free trace experiment.
All 11 page pairs remain unresolved. AcroForm is outside the profile on every
page. Further encountered blockers include lexical numeric provenance in crop
or appearance boxes, annotation/page reference cycles, two image closures over
the expansion budget and one unsupported image decompression path on each side.
These cycles are not a claim that the PDF is malformed: the unrestricted
annotation graph is outside this deliberately restricted paint closure.
Dependencies after an acquisition stop were not evaluated.

The earlier marker-free W-2 projection matched on 11/11 pages, but omitted text
and marked-content execution dependencies. Its exact input and report hashes
were rechecked before reusing that observation. It cannot override the current
validator's source-bound unresolved results. Schedule C and Schedule SE retain
their earlier 0/2 projected page matches as negative trace evidence. Their new
validator runs are also unresolved, including AcroForm/annotation dependencies;
no unsupported result is converted into paint inequality or empty text.

W-2 took 1.38 seconds and 48,604 KiB peak RSS in this single debug-build sample;
its report is 50,600 bytes. Schedule C/SE reports are 10,196/10,287 bytes, with
8,960/7,168 KiB peak RSS. Commands, exit codes and full cost logs are retained.
These costs do not estimate production performance or full-panel applicability.

## Observer binding and adoption limits

The existing public-observer investigation is hash-bound from `summary.json`.
It found that the public renderer does not accept a forwarding `Device`, and
the independent observer lacks native glyph/operator and Form-invocation source
identity. Optional-content state and nested rendering dependencies also require
explicit treatment. This experiment does not invent those bindings or treat
observer callbacks as native source certificates. It acquires its narrow
primitive closure through the existing parser facade instead.

The experiment establishes an exercised sufficient equality validator and a
concrete unsupported W-2 applicability decision. It does not establish universal
impossibility, renderer-wide equivalence, complete PDF interpretation or a new
document-level completion contract. Production adoption would require a separately
approved claim and the actual missing dependency/provenance work. No such
adoption is proposed by these results.

## Reproduction

```bash
cargo test -p pdfdelta-bench --example opaque_effect_probe
cargo run -p pdfdelta-bench --example opaque_effect_probe -- --fixtures
cargo run -p pdfdelta-bench --example opaque_effect_probe -- OLD.pdf NEW.pdf
```

`summary.json` binds the experimental build, mutations, registered inputs,
source-bound applicability reports, costs and reused trace/observer evidence.
