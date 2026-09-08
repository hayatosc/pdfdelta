# Source glyph reductions for local correspondence

These fixtures are filtered captures of the NIST FIPS 186 and Cybersecurity
Framework revision pairs. `provenance.json` records the source PDF hashes,
zero-based pages, selection predicates, fixture hashes, and record counts.
The revision manifest records the original download locations.

Every retained glyph keeps its original ID, mapped text or unmapped identity,
raw character code, geometry, font identity and size, rendering order and
mode, crop and clipping status, and content-stream/operator provenance.
Floating-point geometry is stored as IEEE 754 bits. Vector lines use the
same lossless representation; these selected pages contain none.

The reductions retain the complete pages containing the selected content
and the top portions of neighboring pages. The coordinate predicate is a
fixture selection rule, not a layout threshold in the comparison engine.
The files describe reduced abstract documents: omitted pages are not
represented as extraction failures, and the fixtures do not reproduce the
entire PDF's ambiguity or establish document-wide precision.

The FIPS reduction retains the legacy DSA verification paragraph and its
competing DSA, RSA, and ECDSA passages. With the saved baseline it produces
no established changes, while the DSA note remains an insertion candidate
under an unclosed parent. Its available exact sentence anchors end near
the beginning of the introduction and do not close the note. The reduction
does not preserve the original full PDF's mixed-role classification; that
separate blocker is recorded in the full-document diagnostic capture.

The CSF reduction retains both reviewed scope/functions passages and their
surrounding text. The saved baseline produces no established changes and
supplies no exact recovery anchors to local correspondence discovery.

The local comparison regression fixes five independently checked FIPS
introduction edits to their original glyph IDs:

| Kind | Old glyph IDs and text | New glyph IDs and text |
| --- | --- | --- |
| Replacement | `25995`: `S` | `22058`: `s` |
| Deletion | `26131`: `,` | Absent |
| Replacement | `26650`: `4` | `22517`: `5` |
| Insertion | Absent | `22502..22506`: ` [2]` |
| Replacement | `26765`: `3` | `22664`: `2` |

The half-open insertion range contains four glyphs. The integration test
checks exactly these five events in both revision directions and after
permuting glyph storage while retaining rendering order. The DSA note remains
unrecovered. These fixture expectations do not replace or extend the fixed
real-document benchmark annotations and do not establish whole-document
precision. A separate PDF test interleaves independent content streams and
checks preserved extraction evidence before comparing the resulting edit.

Capture and compare through the normal parser/extraction and comparison
facades:

```sh
cargo run --release -p pdfdelta-bench --example capture_glyph_fixture -- INPUT.pdf 9,10,11 > capture.json
cargo run --release -p pdfdelta-bench --example compare_glyph_fixtures -- fixtures/issue12/fips-old.glyphs.json fixtures/issue12/fips-new.glyphs.json > comparison.json
```

Apply the recorded page/coordinate predicate to the full capture without
changing any retained source record. The fixture loader checks schema,
input-byte, and source-record limits before constructing `Document<Glyph>`.
