# Inline enumeration experiment: rejected

The frozen `enumeration.patch`, `enumeration-source.json`, and accompanying source snapshots describe an experimental extension to the `run-order` implementation. The extension was removed from the library after its scoped benchmark failed to recover either remaining reviewed CSF replacement and introduced an unsupported correspondence.

## Measured result

CSF old/new token coverage changed from approximately 0.677881/0.718028 to 0.680298/0.719272. Changes increased from 790 to 791 and low-confidence changes from 234 to 235. Reviewed recall remained 1/3, with both outstanding replacements still failing. EDPB coverage remained 0.842878/0.853822 with 595 changes and all three reviewed changes recovered.

The added correspondence linked a 303-scalar profile-creation sentence to an 85-scalar glossary definition because both contained a similar ordered function list. Matching an embedded list does not establish correspondence of the surrounding sentences. Increased coverage therefore does not justify adoption.

## Ownership evidence

In the diagnostic source snapshot, the expected old Core sentence is block 166, canonical range 314..430. It already belongs to a deletion. The expected new sentence is block 98, range 0..138, but belongs to a larger insertion spanning blocks 97 and 98, canonical range 8..165. The preceding caption and sentence share that insertion unit. Neither target belongs to a primary match or replacement.

This rules out simply adding a secondary replacement: it would duplicate existing source ownership. Converting exactly matching one-sided units also cannot apply, because the new insertion has a different boundary. Splitting that insertion would preserve evidence only if residual ranges and failure rollback were retained; it would still not prove correspondence of the old and new sentence contexts.

## Decision

Do not weaken ownership checks, suppress competing glossary occurrences, or count the unsupported correspondence as a recovered change. Preserve the existing deletions and insertions. The two reviewed CSF replacements remain unmet. Semantic correspondence inference is outside the currently authorized approach.
