# Literal source annotations, version 2

`pdfbench validate-literal-selectors` resolves source coordinates without running
alignment or reading a comparison report. Version-1 revision annotations and their
whitespace-normalized semantics are unchanged. Version 2 uses the retained raw
text in each source block, with no Unicode or whitespace normalization.

An annotation binds both input SHA-256 hashes, side, literal quote, source view,
normalization convention and locator. `layout_raw` includes explicitly attributed
synthetic spaces and line breaks; `paint_order_raw` includes decoded glyphs in
page-local render order without inferred separators. Paint order is a coordinate
view, not a claim of logical reading order. Preparation metadata names the
existing preparation pipeline; the version-2 selector always addresses its raw
blocks, not canonical text.

Pages are zero-based. `locator.page` restricts to blocks that include that page;
it does not assert that every selected scalar is on that page. `locator.block`
restricts to a block ID. Optional `locator.range` is half-open and requires a
block ID; `position_unit` is either `unicode_scalar` or `utf8_byte`. Byte ranges
must match scalar boundaries and the entire quote. A quote cannot bridge blocks;
use separate selectors for separate blocks. No document-wide flattened diff is
introduced.

Every resolved scalar retains its block-relative scalar and UTF-8 byte offsets
and its original source atoms. Real spaces have glyph atoms; synthetic spaces
and line breaks retain their preceding/following glyphs. Multiple scalars may
refer to the same glyph, including ligatures. Shared source is not split into
invented glyph identities. A unique selector does not certify complete extraction,
counterpart identity, a changed mask or author intent.

Duplicate matches (including overlapping matches) return `ambiguous`, with two
witnesses and no selected source mapping. Hash/view mismatch, unknown glyphs at
or within the quote boundaries, missing source and resource exhaustion remain
unresolved. The command returns 0 when every selector is unique and 3 otherwise;
malformed annotations or failed preparation are errors. Source extraction status
and issues remain separate from selector resolution.

Bounds: 4 MiB annotation input, 1,024 selectors, 65,536 UTF-8 bytes per quote,
32 MiB raw source text per selector, 64 million shared scan/projection work units,
and 65,536 total emitted source atoms. Existing parser/extraction limits also
apply. Budget exhaustion never establishes absence or uniqueness.

## Real-PDF source check

`irs-form-1040-v2.json` selects source text from the existing IRS development pair.
The selectors were prepared from the glyph inspection output, not comparison
results. They are coordinate-validation examples, not a change-recall annotation
or new blind series. The frozen result has three unique selectors (58 scalar
positions) and one deliberately ambiguous repeated form number. The date-field
quote preserves both runs of six actual spaces. The em dash in the third quote
demonstrates different scalar and byte interval lengths. The ambiguous selector
remains unassigned; expected command exit code is 3.

```sh
cargo run -p pdfdelta-bench --locked -- validate-literal-selectors \
  --annotation benchmark/realworld/next/literal/irs-form-1040-v2.json \
  --old benchmark/realworld/cache/irs-form-1040-2024-to-2025-old.pdf \
  --new benchmark/realworld/cache/irs-form-1040-2024-to-2025-new.pdf
```

The PDF provenance and hashes are in `../baseline-3f318d7/inputs.tsv`. Only the
annotation and compact resolved source mapping are checked in. These counts are
not event precision/recall, A/B recovery or extraction completeness measurements.
