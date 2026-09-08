# Unresolved exact-boundary contract

The remaining six strict renderer cells are three existing cases under both
renderers: `text-insertion`, `text-deletion`, and
`numbered-requirement-text-insertion`. Their expected mutation ranges remain
in the strict author-intent metrics; the candidate-policy checks do not make
those strict metrics pass.

The smallest example comes directly from `text-insertion`:

```text
old: A simple release note remains stable
new: A simple release note 2026 remains stable
```

Two different insertion locations produce this same new text:

| Old scalar boundary | Inserted text | Interpretation |
| ---: | --- | --- |
| 22 | `2026 ` | Retain the space after `note`; insert the number and its trailing space. |
| 21 | ` 2026` | Insert a leading space and the number; retain the original space before `remains`. |

Both scripts have five inserted scalars and no deleted scalars. The available
before/after token sequences do not distinguish which space was retained.
The mutation fixture knows which operation it generated; that editing history
is not an input to the PDF comparison engine.

Two requirements therefore need an explicit interpretation together: preserve
the existing strict matrix expectations, and do not establish a location when
competing edit locations remain unresolved. More search or a larger work
budget does not remove the two valid scripts above.

There are two possible contract decisions, neither applied as a specification
change in this work:

1. Require a uniquely supported scalar boundary. These cells remain candidates
   with proven content change, and the release expectations must explicitly
   acknowledge that limitation.
2. Define exact output coordinates through a documented canonical convention
   for equivalent whitespace-boundary scripts. This requires deciding which
   differences are equivalent and preserving genuine repeated-content
   ambiguity as unresolved; a deterministic tie-break alone is not evidence
   that the original editing location was unique.

The current implementation follows the first interpretation. Its 42/48
strict result is a recorded release-condition failure, not authorization to
change the expected release contract.
