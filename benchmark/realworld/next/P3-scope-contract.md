# Closed retained-order interval comparisons

Status: initial implementation, not a completed real-document adoption result.

`closed-retained-order-interval-v1` defines a finite observable interval between
two unchanged, accepted source-only mandatory one-to-one boundaries in an existing
corresponding parent. It reuses the document scope, source children, precedence
relations, local group comparison and normalization proof. It does not establish
semantic paragraph identity or recover the author's editing history.

The selected parent children must form one complete, source-backed precedence
chain on each side. Every member has text content, explicit source-structure provenance,
nonempty disjoint source references and complete text acquisition inventory.
Detached members, incomplete relations, acquisition issues, intersecting source
conflicts and alternative partitions prevent this closure. Boundary proposals
must already have accepted source-only mandatory decisions; optional interior
candidate enumeration may remain incomplete. The review records that search
status and does not resolve interior identities. Two low-frequency anchors alone
do not meet these requirements.

`closed-native-baseline-interval-v1` additionally admits separate native order
runs when the complete parent order is unavailable. Discovery follows unbranched
native paragraph precedence edges. On each side, the inclusive path must lie on
one page with complete text acquisition, strictly descending nonoverlapping
horizontal baseline ranges, disjoint native sources, and no intersecting source
conflicts or alternative partitions. Every native glyph whose baseline lies in
the inclusive vertical interval and whose box overlaps the path's horizontal
extent must belong to the path. This source scan catches omitted or detached
material; unrelated pages do not invalidate it. Path glyphs must be inside the
CropBox, unclipped or inside the explicit path clip, and use fill/stroke paint
modes. This is a retained-text convention, not a claim about transparency or
later overpainting. It does not admit rotated text or a native cross-page range.

Adjacent old boundaries must preserve new order without another accepted boundary
inside the new interval. Both interiors must be nonempty, within the existing
group-node bound, and share a declared text kind. A bounded local group comparison
must establish a content change. The initial implementation deliberately leaves
one-sided ranges and cross-kind comparisons unresolved. The complete-parent
convention rejects non-text siblings; the native convention checks its own finite
source band without requiring unrelated channels to form the same chain.

Each `text_scope_reviews` entry preserves the parent scope, boundary proposal
indexes, complete interior and boundary source references, member node IDs,
comparison convention, local result and conditional masks. Input revision hashes
bind the report's source references. These source lists include unchanged context;
they are not changed masks. An inferred parent produces C, never B. Otherwise the
established interval content difference is B, even when no unique changed-source
positions exist, such as `a -> aa` or one copy of `x` becoming two copies.
Individual interior counterpart rivals remain in the original matching result.

Reviews are excluded from the strict comparison iterator, source ownership,
coverage and process-exit decision. CLI JSON and text distinguish B and C counts
from the existing typed and inferred operations. Conditional masks are retained
within each review without being merged into A. Display previews are excerpts;
complete operation text and source lists remain in JSON.

Closure work uses a separate bounded allowance from the existing ownership limit;
local proof uses existing local limits. Exhausting this optional search emits no
review and does not claim complete scope-review recall. Its skipped attempts do
not yet have a separate diagnostic ledger. The existing strict search status does
not certify completeness of these optional reviews.

Tests cover ambiguous insertion positions, nonexact splits, reversed comparison,
reversed source storage, duplicate content, detached members, repeated boundaries,
missing inventories, incomplete relations, crossed boundaries, acquisition issues,
local-proof limits and inherited parent inference. They verify that source range
references exclude boundary context and that adding reviews leaves strict coverage
unchanged. Independent PDF recovery, concrete cross-kind handling, static source
navigation and family-level evaluation remain required before full adoption.
