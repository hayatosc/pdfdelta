# Shared work accounting for native domains

## Result

The native-domain proof phase now passes its remaining work directly to the
existing local comparator. It no longer reserves a `(left + 1) * (right + 1)`
grid estimate. The exact comparator already has a linear equality proof: the
old reservation both overcharged long equal bodies and underfunded short ones.
For one token on each side, the old allowance was four units while the kernel
requires ten. No proof-work limit, ownership condition or completion predicate
was increased or weakened.

| Pair | Old compared sources | New compared sources | Gain per side over previous implementation |
| --- | ---: | ---: | ---: |
| EDPB controller/processor | 2,154 | 2,180 | 54 |
| EDPB restrictions | 893 | 893 | 296 |

Each pair now has 39 verified native domains. Schedule C, Schedule SE and NIST
contingency remain unchanged. All five pilot comparisons remain incomplete;
this is not a full-panel completion evaluation or a newly completed pair.

## Contract and checks

The public local-comparison API retains its independent budget per call. A private
entry point accepts the domain phase's shared remaining work. Successful and
failed comparator work stays charged, and subsequent calls cannot refill it.
All previous domain closure, exact normalization, source projection, shared-glyph,
local inventory, correspondence and complement requirements are unchanged.

A one-character boundary regression now produces the two valid owned domains.
A separate test exhausts a shared budget across calls and verifies that another
call remains unresolved while a new public call has its own declared budget.
Workspace tests pass: 2,495 passed, zero failed, two ignored. Clippy, formatting
and whitespace checks pass. Generated fixtures pass 48/48, including the existing
42 strict author-intent and six candidate-policy cases.

The five-pair pilot preserves every previous candidate, strict comparison, B
review, boundary correspondence, unresolved reason and native domain. Only the
newly established domains and their source coverage are added. Current reports,
source archive, binary and check logs are hashed in `domain-work-pilot.json`.

`audit_native_domains.py` independently reconstructs all 78 reported domains from
the preceding frozen native source bundles, checking exact native text, projection,
source uniqueness, complements and indivisible ownership. The input revisions
and native counts match. This reuses retained source acquisition; it is not a
fresh source extraction or an independent proof of domain admission, visible-text
inventory or completion. Results are in `domain-work-controller-review.json` and
`domain-work-restrictions-review.json`.

## Remaining work

Inventory and search obligations remain independent. This correction removes an
implementation error in the strict source path; it does not justify dismissing
uninterpreted paint or establishing paragraph identity from boundary equality.
The goal of at least two newly complete natural pairs remains active.

## Subsequent projection validation

The historical counts above are not the latest verified coverage. See
`domain-projection.md` for the added native-reading/order checks, current source
counts and references returned to unresolved under the unchanged work budget.
