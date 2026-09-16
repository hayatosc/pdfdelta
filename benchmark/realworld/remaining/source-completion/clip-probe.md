# Empty-clip applicability preflight

## Decision

Do not prioritize an empty-execution-clip exception as the next completion fix.
The fixed panel produced only 21 such paint-record attempts, across four documents
in three pairs, and every affected page also had other paint attempts. No new
complete inventory or comparison is established. Other visibility proofs and
text acquisition remain necessary; this measurement does not classify the other
paint as visible text or decoration.

## Method and checks

A diagnostic-only patch logs the current clip at the existing native paint-record
function, including nested execution and opaque failure replacement. It changes
no inventory, source or search predicate. The instrumented executable is preserved
separately, and the production source and ordinary release executable were restored.
The patch applies to the source archived by the preceding native-domain pilot.

Three generated PDF controls verify an ordinary fill, an already empty clip, and
pending-clip timing plus graphics-state restoration. The latter observes false,
true, false: the current paint precedes a pending clip update, and `Q` restores
the caller's clip. These are execution controls, not rendering-equivalence tests.

The driver checked all 72 fixed input hashes, bypassed the extraction cache, and
called the bounded native worker with its original limits and a 35-second outer
timeout. All processes exited zero; 71 returned typed successful responses. ECB
annual 2023 returned a typed resource limit: CID width entries exceeded 65,536.
A repeat returned the identical error-response hash. It remains in the denominator.
Successful responses can retain acquisition issues and are not complete-inventory
certificates. Instrumentation timings are not production performance measurements.

## Observations

The logs contain 446,275 paint-record attempts, including 21 under an empty clip.
Attempts can include partial Form effects later rolled back; these counts must
not be presented as distinct retained source references or final inventory counts.
Page indices below are zero based.

| Document | Empty attempts | Affected pages | Other attempts on those pages |
| --- | ---: | --- | ---: |
| MEXT primary Japanese, new | 2 | 13 | 1 |
| Mask R-CNN, old | 1 | 0 | 101 |
| Mask R-CNN, new | 2 | 0, 8 | 740 |
| Bunka kana, new | 16 | 0–5, 7, 10 | 534 |

The empty-only-page count in these observed attempts is zero. This is a narrow
applicability result, not a proof that no invisible effects exist elsewhere.
Unknown clips, paint outside a nonempty clip, opacity, compositing and unexecuted
or truncated regions remain outside this diagnostic.

## Evidence

`clip-probe-results.json` records input, binary, patch, driver and log hashes,
individual source/operator observations, generated controls and the typed failure.
`clip_probe.py` reruns the measurement with an explicitly supplied instrumented
binary. `clip-probe.patch` is diagnostic source, not applied production behavior.
Raw logs, generated PDFs, the executable, instrumented file and build log are
preserved in the ignored `benchmark/realworld/cache/source-completion/clip-probe-v1/`.
The previously validated production implementation is unchanged.
