# Invocation-selected resource acquisition

## Result and boundary

The benchmark profile `opaque-invoked-resources-v2` acquires only explicitly invoked XObjects and extended graphics states at each page/Form execution scope. Unused font, image and state entries, including a Form's unused self-reference, no longer prevent this sufficient relative paint equality proof. Actual image contents, Form programs, matrices, bounds and invoked recursion remain dependencies.

This is an experimental acquisition improvement, not production text inventory, spatial isolation of individual effects or complete comparison. The entire supported page program, entry state, backdrop, exact command bytes and relevant page context remain part of equality. No glyph/operator-to-renderer binding or recognition provider is introduced. The production completion contract is unchanged and no pair becomes complete.

## Retained implicit and unsupported dependencies

A resource can affect execution without being named by `Do` or `gs`. The reader therefore checks the scope's color-space dictionary for DefaultGray, DefaultRGB and DefaultCMYK and refuses these implicit overrides. It does not silently drop a default color-space change. Unsupported text, pattern, marked-content and other operators continue to fail program validation. Invoked state dictionaries still reject masks, unsupported blending and unknown settings.

Resource dictionaries use the neutral parser facade and bounded reference resolution. Selection visits and names consume the same closure work/byte limits, and nested invocation obeys the same depth limit. A resource reference that is never invoked does not constitute execution recursion; a Form invoking itself remains unresolved. Existing annotation/page back-references and AcroForm execution are still outside this experiment.

## Verification

Thirty independently varied conditions pass: the original 24 controls plus four positive unused-resource controls and two negative default-color-space controls at page and nested Form scope. Existing controls retain changed image bytes behind unchanged commands, caller/Form transforms, clips, blend, masks, lexical decimal collisions, missing resources, and actual recursion. The unit suite has three tests, with the mutation test asserting every condition.

The frozen W-2 capture verifies both registered input hashes and retains all 11 pages unresolved. AcroForm and annotation dependencies remain; no natural opaque equivalence or text completion is claimed. A first launch of the copied executable lacked its execute mode and did not run; the mode was restored from the built executable before the recorded capture.

Workspace tests pass with 2,515 passed, zero failed and two ignored; formatting and all-target Clippy pass. `invoked-resources-v2.json` binds the source archive, executable, 30-condition observations, W-2 commands/raw reports and logs. The original v1 evidence remains historical. Broad spatial effect isolation, actual source/observer binding and interpretation of remaining drawing are still required work.
