# `pdf_oxide` 0.3.77 Extraction Oracle

This directory contains a curated, position-aware glyph snapshot for
[`case1-japanese-typst/old.pdf`](../../external/case1-japanese-typst/old.pdf).
The snapshot was produced outside the pdfdelta workspace with
[`pdf_oxide` 0.3.77](https://github.com/yfedoseev/pdf_oxide), whose custom PDF
parser is independent of pdfdelta's `lopdf` backend. The workspace deliberately
does not depend on the oracle producer at build or test time.

The producer used `PdfDocument::extract_chars` and converted each returned
`TextChar` as follows:

- the returned page and character order become zero-based `page` and global
  `render_order`;
- `char` becomes mapped text;
- `origin_x` and `origin_y` become the baseline;
- the bounding box spans `origin_x .. origin_x + advance_width` horizontally
  and `origin_y + descent .. origin_y + ascent` vertically;
- direction is `(1, 0)` because the source fixture contains only horizontal,
  unrotated text.

The fixture is intentionally limited to 158 mapped horizontal glyphs. It is one
auditable cross-parser check, not evidence for unsupported writing modes or
unmapped glyphs. Producer identity remains declared provenance rather than an
automatic independence certificate.

Verify it from the repository root:

```bash
mise run bench-extraction-conformance -- \
  fixtures/external/case1-japanese-typst/old.pdf \
  --oracle fixtures/extraction-conformance/pdf-oxide-0.3.77/case1-japanese-typst-old.oracle.json \
  --geometry-tolerance 0.25
```

| File | Size (bytes) | SHA-256 |
|---|---:|---|
| `case1-japanese-typst-old.oracle.json` | 42491 | `81c6abb80a2e1a8071a249dd5c818fcd19d0accff9d9c80ad135ccd4d43ff8ed` |
| `../../external/case1-japanese-typst/old.pdf` | 26681 | `47fd47f6dd6a1316612880a76af15b4dfec121b182dbde789578b17e1e3c35ab` |
