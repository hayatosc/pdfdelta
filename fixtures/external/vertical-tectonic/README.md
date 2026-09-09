# External Japanese vertical-text controls

These small PDFs were generated with Tectonic 0.16.9 and Noto Sans CJK JP.
The source uses XeTeX's `vertical` font feature and a counter-rotated box,
following the [XeTeX author's vertical-typesetting example](https://tug.org/pipermail/xetex/2007-July/006959.html).
The extracted body has ten downward glyph advances (`direction=(0,-1)`),
alongside a four-glyph horizontal heading. The embedded font subsets let tests
run without Tectonic, TeX packages, or host fonts.

- `vertical-old.pdf` contains `出荷重量百キログラム`.
- `vertical-moved.pdf` preserves the text, starts the body on a new page, and
  changes its horizontal position.
- `vertical-changed.pdf` changes the body to `出荷重量二百キログラム`.

The hash-bound annotations were authored from these source values before
comparison. They score change units, independently of page-rendering changes.
The CLI regression additionally checks retained vertical direction, native glyph
counts, unchanged-text coverage after movement, and the exact inserted-source
mask: new glyph 8 (`二`), at local scalar position 4, with no old-side deletion.
The full value is the review unit; the other characters are not marked changed.

The movement control compares all 14 native glyphs per input with no text
change. The changed control reports one inferred text change; it preserves 14
old and 15 new glyphs, while source-established text coverage is only 4/4.
Both four-channel comparisons remain incomplete and exit 3. Evaluation exits 1;
these controls do not prove complete relationship or visual interpretation,
ruby, multiple vertical columns, or arbitrary Japanese punctuation handling.
These are development fixtures, not unseen holdouts.

## Regeneration

Install the producer and font family above and prepare a Tectonic cache with
`article`, `fontspec`, `graphicx`, and `geometry`. Copy the `.tex` sources to a
new working directory, then run there:

```bash
for name in vertical-old vertical-moved vertical-changed; do
  SOURCE_DATE_EPOCH=0 TECTONIC_CACHE_DIR=/path/to/prepared-cache \
    tectonic --only-cached --untrusted --outdir . "$name.tex"
done
```

The normal test run uses the vendored PDFs. Regeneration depends on the stated
producer, font resolution, and cached package versions; the hashes below identify
the evaluated inputs.

| PDF | Bytes | SHA-256 |
| --- | ---: | --- |
| `vertical-old.pdf` | 4317 | `036db9c5463405babc019517345b94f6cf512c71382b86f9fc2839310c3989a3` |
| `vertical-moved.pdf` | 4484 | `0ff12ad3a30a38ab36dd932e2f3ba99ea0e546419f95fa81b289ce6851b6e3cf` |
| `vertical-changed.pdf` | 4357 | `33545f15114907ba5b973a51bd9f496fad36ba930821c60a5f07047dfa65f392` |
