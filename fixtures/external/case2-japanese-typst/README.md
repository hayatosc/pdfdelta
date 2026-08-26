# External Fixture: SPEC §2.1 / §2.2 Case 2 Japanese Page-Break-Only PDF (Typst)

## Overview

This directory vendors an externally rendered Japanese PDF revision pair evaluating **SPEC §2.2 Case 2** (page-break-only changes producing zero content changes) extended to **SPEC §2.1** (born-digital horizontal Japanese text with single-column layout). It provides an independent, real-world compiler dialect (Typst) and CJK font embedding (Noto Sans CJK JP subsets) without requiring external rendering tools at test runtime.

The text payload between `old.typ` and `new.typ` is 100% byte-identical; only an explicit `#pagebreak()` declaration before the final paragraph in `new.typ` differs, moving the final paragraph across a page boundary to page 2 (zero-indexed page 1) without changing any glyphs, fonts, margins, or line-wrapping.

## Provenance & Environment Assumptions

- **Renderer**: `typst 0.15.1 (9dfd3a08)` (see [`fixtures/external/japanese-typst/README.md`](../japanese-typst/README.md) for toolchain details).
- **Font Package**: Debian/Ubuntu package `fonts-noto-cjk` version `1:20230817+repack1-3` (upstream release tag `Sans2.004`; note: font path `/usr/share/fonts/opentype/noto` is Debian/Ubuntu-specific).
- **Embedded Font Files Used by Typst**:
  - `/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc` (size: 19,484,784 bytes, SHA-256: `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a`) — used for body paragraphs.
  - `/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc` (size: 20,050,760 bytes, SHA-256: `faa5f3656a78b2e2d450d27fe8382c778bc2b6bb5ea29c986664a6a435056ceb`) — used for headings.
- **Font Copyright & License**: SIL Open Font License, Version 1.1 (see [`THIRD-PARTY-NOTICES.md`](../../../THIRD-PARTY-NOTICES.md)).
- **Target SPEC Case**: §2.2 Case 2 (page-break-only change with 0 content changes) in Japanese.
- **Redistribution**: Safe public domain synthetic test text; contains no personal data, machine-specific paths, or secrets.

## Repo-Root-Complete Generation Commands

```bash
# From repository root (assumes typst 0.15.1 and Debian/Ubuntu fonts-noto-cjk package installed at /usr/share/fonts/opentype/noto)
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case2-japanese-typst/old.typ fixtures/external/case2-japanese-typst/old.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case2-japanese-typst/new.typ fixtures/external/case2-japanese-typst/new.pdf
```

## Deterministic Verification Recipe

Run this recipe from the repository root to verify byte-for-byte reproducibility:

```bash
# 1. Render old.typ twice to temporary files
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case2-japanese-typst/old.typ /tmp/old_run1.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case2-japanese-typst/old.typ /tmp/old_run2.pdf

# 2. Render new.typ twice to temporary files
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case2-japanese-typst/new.typ /tmp/new_run1.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case2-japanese-typst/new.typ /tmp/new_run2.pdf

# 3. Verify byte-for-byte reproducibility across runs and match vendored PDFs
cmp /tmp/old_run1.pdf /tmp/old_run2.pdf
cmp /tmp/old_run1.pdf fixtures/external/case2-japanese-typst/old.pdf
cmp /tmp/new_run1.pdf /tmp/new_run2.pdf
cmp /tmp/new_run1.pdf fixtures/external/case2-japanese-typst/new.pdf

# 4. Clean up temporary files
rm /tmp/old_run1.pdf /tmp/old_run2.pdf /tmp/new_run1.pdf /tmp/new_run2.pdf
```

## Checksums (SHA-256)

### Fixture Files

| File | Size (bytes) | SHA-256 Checksum |
|---|---|---|
| `old.typ` | 454 | `8aa90b3bc90c364e0409e2b1ff0cc8b025bb8fce6d50394cfadc369d61d3655a` |
| `new.typ` | 468 | `d3caf65abb70773fc127521df0e18ca80b8c44d8677e9909e28be894344c123b` |
| `old.pdf` | 21701 | `fdd1af7efbaaf43961b655faecc71a935081884cf751ac9e9fd5581a989eda43` |
| `new.pdf` | 22238 | `0a08f413213b2500128ddaf1192bfd2db8d6b9942623d804d56198ab335925eb` |

### Upstream Font Files

| Font File | Size (bytes) | SHA-256 Checksum | Role |
|---|---|---|---|
| `NotoSansCJK-Regular.ttc` | 19484784 | `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a` | Body text |
| `NotoSansCJK-Bold.ttc` | 20050760 | `faa5f3656a78b2e2d450d27fe8382c778bc2b6bb5ea29c986664a6a435056ceb` | Heading text |

## Expected Outcome

- **Process Exit Status**: `0` (`ExitStatus::Unchanged`) in both default and `--strict` modes
- **Content Changes**: Exactly `0`
- **Formatting Changes**: `0`
- **Uncertain Changes**: `0`
- **Unresolved Regions**: `0`
- **Extraction Complete**: `true` on both `old` and `new`
- **Extracted Glyphs**: Exactly 100 glyphs per side, all horizontal direction `(1.0, 0.0)`, all mapped Unicode scalars
- **Page Provenance**: `old` has 1 page (all 100 glyphs on page 0); `new` has 2 pages (72 glyphs on page 0, 28 glyphs on page 1)
- **Reconstructed Blocks**: 4 blocks on both sides; blocks 0-2 on page 0 on both sides, block 3 on page 0 in `old` and page 1 in `new`
- **Comparison Complete**: `true`
- **Alignment Coverage**: `1.0` (100.0%) on both sides
