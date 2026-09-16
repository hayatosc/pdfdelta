# Declared text and artifact tags around paint

A bounded extension to the existing paint trace probe records inline `ActualText` declarations, artifact tags, unresolved named properties and marker balance around page-program paint operations. It preserves every observed paint operation. These are source-syntax observations, not text acquisition, visibility, interpretation or comparison certificates.

The six fixed W-9, Schedule C and Schedule SE inputs contain 1,215 observed page-program paint operations across 20 pages. None has an inline `ActualText` declaration or unresolved named property in its enclosing marked-content frames. No inline declaration occurs elsewhere in those page programs either. Marker stacks are balanced and the parser reports no issues in these captures.

| Pair / side | Paint operations | Artifact-tagged paint |
| --- | ---: | ---: |
| W-9 old | 155 | 0 |
| W-9 new | 162 | 160 |
| Schedule C old | 309 | 304 |
| Schedule C new | 309 | 299 |
| Schedule SE old | 140 | 133 |
| Schedule SE new | 140 | 133 |

The 1,029 artifact-tagged operations remain unresolved evidence. A producer's artifact declaration cannot by itself establish the absence of visible lettering. The selected-text CLI regression now exercises a visible outline both with and without an artifact wrapper; both retain zero native glyphs and incomplete text inventory. The marker does not turn either input into successful empty text.

The diagnostic controls retain a direct declaration, flag its invalid numeric value type, distinguish a named property from a resolved dictionary, preserve artifact paint, report unbalanced markers and reject marked-content nesting beyond 64. Existing page-byte, operator and parser limits remain enforced. The probe keeps `certifies_text_inventory: false`.

## What this changes in the investigation

A provider based solely on inline replacement text has no candidates for the retained paint in these six page-program captures. This rules out that narrow acquisition route here; it does not rule out all source-backed acquisition. The probe does not resolve structure dictionaries, named properties, invoked Form programs or annotation appearances. It does not prove that metadata elsewhere is absent, or that every drawing is or is not text. No OCR output or confidence score is promoted to strict coverage.

Production comparison behavior is unchanged. No inventory obligation, strict-source residual, search obligation or newly complete pair is discharged. The 0/36 completion objective remains unmet. Future acquisition work needs source-bound interpretation and complete region coverage; tags or relative drawing equality alone are insufficient.

`paint-properties-v1.json` binds all six input hashes, raw observations, commands, executable, source snapshot and checks. The new CLI control is also retained separately because it was added after the probe capture. This is not a rerun of the full comparison panel or an independent rendering adjudication.
