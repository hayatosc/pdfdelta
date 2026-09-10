# Rotated distribution-stamp source reduction

These two small fixtures retain the original glyph records for the vertical
distribution stamp on page zero of the Attention revision pair. The selection
predicate is page zero and `direction.x == 0.0`; it is used only for this fixture,
never for production candidate discovery. Original glyph IDs, raw codes,
coordinates, font identity, render order, rendering mode, and operator provenance
are unchanged. Input and fixture hashes are recorded in `provenance.json`.

Capture page zero through the existing parser/extractor facade:

```sh
cargo run --release -p pdfdelta-bench --example capture_glyph_fixture -- INPUT.pdf 0 > /tmp/attention-page.json
```

Retain the selected glyph records without changing their contents. These are
reduced, stamp-only abstract documents; they do not establish correspondence or
uniqueness in the full PDFs. The full revision benchmark remains separate.

The fixed literal-minimal source mask owns old glyphs `2442`, `2455`, `2457`,
`2459` and new glyphs `2442`, `2456`, `2458`. The common month letter and adjacent
spaces remain equal. The integration test checks these seven positions in both
directions and with glyph storage permuted while preserving rendering order.
The frozen full-document annotation still expects eleven changed tokens; this
fixture does not replace that annotation or claim its complete recovery.
