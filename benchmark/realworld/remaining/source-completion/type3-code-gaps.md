# Localize missing Type 3 procedures to the referenced code

## Problem and change

The native simple-font loader rejected an entire Type 3 font when any Encoding Differences name was absent from CharProcs. An unused missing entry therefore prevented decoding another code whose definition was present. The regression in commit `7d24366` reproduces rejection of defined code A when only B lacks a procedure.

Encoding parsing now retains a per-entry missing-procedure flag. Decoding any flagged code returns unresolved, including a code mapped by ToUnicode. The error is not converted to an empty glyph. Other defined entries may decode normally. Stable Type 3 identity bindings omit missing entries; absence cannot manufacture a glyph identity. Malformed encoding syntax, ambiguous identity, invalid existing procedures and resource limits retain their existing treatment. A text-show operation containing a missing code can still fail as a unit; this change does not claim partial recovery inside that operation.

The private decoder retains at most one Boolean per already bounded encoding entry. The acquisition cache format is 17 and the native profile is `content-stream-v13-type3-code-gaps-worker-v1`, preventing reuse of previous whole-font failures.

## Natural evidence

The frozen two-pair pilot uses unchanged input hashes and budgets. GPT-3 old/new native glyph counts increase from 183,113/188,326 to 194,640/198,601. All eight missing-CharProc text issues disappear. Compared source counts rise from 119,031 to 128,385 on each side. Newly discovered sources also increase residuals to 66,255/70,216; recovered acquisition is not itself complete comparison.

The Schedule SE control retains exactly the same comparison and coverage. Both pairs remain incomplete, and no new fixed-panel completion is claimed. `type3-code-gaps-audit.json` verifies source conservation and simultaneous inventory/source/search obligations from the raw reports. Additional natural readings and masks have not been independently adjudicated. The next work still requires drawing interpretation and source-backed comparison of residual regions.

## Validation and reproduction

The initial regression fails solely on whole-font rejection; formatting and Clippy pass at that reproduction commit. Twelve Type 3 unit tests pass after the change, including used missing codes and a ToUnicode declaration that cannot supply absent drawing. Final workspace and generated verification results are registered in `type3-code-gaps-checks.json`. Binary, source archive, build conditions and raw captures are bound by `type3-code-gaps-v1-pilot.json` and retained in the ignored frozen cache. Production Rust code remains within the existing neutral parser and native worker boundaries.
