# H17 simple-font selector identity for explicit encodings

Status: accepted candidate. Final full-panel capture is
`h17-full-iteration-008-native` (binary `a297b88f5d9e…`, patch `2207404e4795…`,
source archive `71bdc122fc87…`, summary `c5d947261188…`), 36 captured /
0 failed / 3 complete. The seven required gates passed on this source in
`h17-gates-final2.log.gz`. NASA extraction is now complete, but the pair is
not comparison-complete; score stays 3/36.

## Production change

`FontIdentitySourceKind::EmbeddedWithSelector` gives simple fonts with a
proven explicit encoding a stable identity: a disjoint leading family tag
(`embedded-with-selector\0`) is hashed before `domain.tag()`, then a
length-prefixed canonical selector (entry count, base encoding identity name,
and `(code, name length, name)` per Differences entry), then the embedded
program bytes. Existing `Embedded` hashing is unchanged. The selector is
preflighted against the bytes remaining after ToUnicode and charged at
construction with the ToUnicode map, so mapped-only fonts cannot skip it;
identity loading later charges only newly decoded program bytes. Unknown or
private bases, ambiguous Differences, MMType1, invalid flags and missing
programs keep the blanket refusal.

Source evidence for raw `0x09` under `/WinAnsiEncoding`: ISO 32000-1:2008
9.6.6.4 and Annex D.2, plus pdf.js `src/core/encodings.js` (URL
`https://raw.githubusercontent.com/mozilla/pdf.js/master/src/core/encodings.js`,
content SHA-256 `eee7b0b49fbf0c27fd1765abeea43621c23da3d1a8a592ad0e5ddfe6743658e3`,
latest commit `81525fd446779f0a29003cf06e3ba33751954826`), whose
`WinAnsiEncoding[9]` entry is empty. That proves the absence of a glyph
name/Unicode mapping; it does not prove a rendered result, so raw `0x09` stays
preserved opaque evidence and equality is justified by binding every selector
input (program, encoding, code).

## Verification

- Unit/integration: lib 1,383 passed / 2 ignored, all-features 1,402 passed /
  2 ignored, `content_stream_extraction` 123 passed, `pdfbench verify` 48/48;
  the `mapped_only_font_selectors_charge_the_shared_byte_budget` consumer
  regression fails at total 40 on the second selector
  (`simple-font selector identity bytes`) and passes at 50, with
  `decoded_calls == 0` and exact `Mapped("A")` glyphs.
- Gate log with start/end UTC, exact commands/exits and hashes of the four
  changed Rust files: `gates-final2.log.gz`.
- Raw audit (`raw_glyph_audit_sha256` in `full36-manifest.json`): complete
  `model::Glyph` streams from both bound source archives; 280,452 of 280,452
  prior glyphs retained in exact stable-field order, 0 lost, 109 added on
  page 85 / content stream 829; `id` offset 0/109; `render_order` offset
  0/2513 (shared allocation with vector/paint and truncated `Do` invocations,
  see `render_order_offset_explanation`; whole-page restoration is not proven).
- Projection audit: 280,440 retained + 109 added, 0 lost, 0 id conflicts.
- Endpoint audit: 2,887 synthetic_space/line_break endpoint pairs on both
  sides, all mapped through the proven id map, 0 unmapped / 0 missing /
  0 added; the same 12 raw glyphs are excluded from the projected inventory on
  both sides.
- Region grouping: 29 unresolved new-side regions (47,979 comparable tokens)
  all join relations; `evidence` is empty on every region, so causes come from
  relations: `normalization_uncertainty` 87, `domain_not_closed` 58,
  outcomes `tentative` 87 / `established` 29, all searches complete,
  work exhausted exactly at the 32,000,000 limit (localization 15,833,460,
  emission 15,036,439, anchor_verification 1,130,101). Extraction complete is
  distinct from comparison complete.

## Preserved failures and limitations

- `retention-nasa.json` stays `fail` (683 problems). The generic audit
  assumes identical inventory; NASA's new side gained 2,628 changes, so its
  40,616 missing / 40,725 new comparable keys and 75,823/75,696
  source-glyph counts describe repartitioning, not loss. It is not waived,
  and its zero `sources_missing` is vacuous because the candidate counts were
  zero.
- `205,451` is a transition count of shared resolution keys from unresolved
  to changed, not restored extraction tokens; net new comparable tokens are
  109.
- `raw-glyph-audit-failed-prefix.json` preserves the failed positional
  comparison (matched prefix 204,822, lost 1, added 75,739).
- Remaining: endpoint causes for synthetic space/line-break are proven equal
  but not yet attributed to individual normalization events; the next
  falsifiable hypothesis is that NASA's residual tentative relations are
  normalization/closure-bound around the recovered page-85 run.
