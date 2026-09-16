# Inventory contract decision

Status: the user approved adding a separately versioned observation completion
indicator on 2026-09-16. Implementation is pending; no completion predicate or
acquisition policy has changed. Existing strict completion remains separate.

## Established boundary

`inventory-contract-v1.json` binds the current source and rechecks the retained
36-pair panel plus the five-pair lookup-fix pilot. All 72 inventories in the full
panel are incomplete; all five latest pilot pairs remain incomplete. This is an
audit of saved observations, not a new full-panel execution.

The production path has three separate constraints:

- `document/native.rs` leaves native text inventory incomplete on pages retaining
  non-text paint. Native extraction alone does not inspect text carried by images
  or paths.
- `document/providers.rs` assigns OCR observations a recognition basis;
  `document/graph.rs` classifies that basis as inferred.
- `document/coverage.rs` excludes inferred comparisons from strict source coverage.
  `recognition_inventory_and_confidence_cannot_discharge_strict_sources` tests equal
  and changed readings, declared complete/incomplete inventories, and confidence
  zero/100. None discharges strict recognized sources.

These are intentional boundaries, not a missing confidence threshold. Improving
detector postprocessing or recognition accuracy does not itself provide exhaustive
acquisition or exact source interpretation. Exact rendering dependency equality
also does not identify the characters inside an opaque effect. Future independently
justified acquisition proofs remain possible; this audit does not prove them
impossible or authorize treating recognition as certain.

## Approved separate observation contract

The approved extension will retain four distinct outcomes for a source-bound
region:

1. Text interpretation and comparison proved under the existing source contract.
2. Rendering-effect equality proved under an explicit execution profile, with
   text interpretation still unresolved.
3. Appearance difference established under a declared rendering profile, with
   exact text interpretation still unresolved.
4. Unresolved acquisition, correspondence, execution or comparison obligations.

Dependency inequality alone is not appearance difference. Equal low-resolution
rasters alone are not rendering-effect equality. Matching regions, dependencies,
limits, source ownership and the declared observation profile must be retained.
Unknown effects that may interact with a region keep it unresolved; empty or
missing acquisition is never a completed region. Recognition alternatives and
their uncertainty remain visible regardless of the observation result.

An observation completion field would require exhaustive accounting under its
own explicit profile and zero unresolved obligations for that profile. It would
not set `comparison_complete`, erase text inventory gaps, change existing A/B/C,
or be reported as an improvement in the old strict completion count. It offers
no guarantee that two fixed natural pairs can meet the new contract either.

Adding this indicator would not, by itself, satisfy the active goal of two newly
complete strict comparisons. Changing that goal would require a separate explicit
user decision. Until then the existing goal and both temporary plans remain;
no opaque or OCR result is promoted into strict coverage.
