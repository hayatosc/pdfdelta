# External Fixture: Japanese Horizontal Born-Digital PDF (Typst)

## Overview

This directory vendors an externally rendered Japanese PDF revision pair evaluating born-digital horizontal Japanese support with single-column layout and exact text replacement (`第10版` -> `第20版`). It provides an independent, real-world compiler dialect (Typst) and CJK font embedding (Noto Sans CJK JP subsets) without requiring external rendering tools at test runtime.

## Provenance & Environment Assumptions

- **Renderer**: `typst 0.15.1 (9dfd3a08)`
- **Font Package**: Debian/Ubuntu package `fonts-noto-cjk` version `1:20230817+repack1-3` (upstream release tag `Sans2.004`; note: font path `/usr/share/fonts/opentype/noto` is Debian/Ubuntu-specific).
- **Embedded Font Files Used by Typst**:
  - `/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc` (size: 19,484,784 bytes, SHA-256: `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a`) — used for body paragraphs.
  - `/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc` (size: 20,050,760 bytes, SHA-256: `faa5f3656a78b2e2d450d27fe8382c778bc2b6bb5ea29c986664a6a435056ceb`) — used for headings.
- **Font Copyright & License**:
  - Copyright: `© 2014-2021 Adobe (http://www.adobe.com/).` and `Copyright 2010-2012, Google Corporation`
  - License: SIL Open Font License, Version 1.1 (see [`THIRD-PARTY-NOTICES.md`](../../../THIRD-PARTY-NOTICES.md))
  - Authoritative license notice file: `/usr/share/doc/fonts-noto-cjk/copyright` (size: 14,712 bytes, SHA-256: `849f4ea9c214fa4ac3593b770c699f387534b11ce671264c1b10d85bdcb5997b`)
- **Target Evaluation Case**: Japanese horizontal born-digital PDF text replacement (`第10版` -> `第20版`, character change `'1'` -> `'2'`).
- **Redistribution**: Safe public domain synthetic test text; contains no personal data, machine-specific paths, or secrets.

## Repo-Root-Complete Generation Commands

```bash
# From repository root (assumes typst 0.15.1 and Debian/Ubuntu fonts-noto-cjk package installed at /usr/share/fonts/opentype/noto)
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/japanese-typst/old.typ fixtures/external/japanese-typst/old.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/japanese-typst/new.typ fixtures/external/japanese-typst/new.pdf
```

## Deterministic Verification Recipe

Run this recipe from the repository root to verify byte-for-byte reproducibility:

```bash
# 1. Render old.typ twice to temporary files
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/japanese-typst/old.typ /tmp/old_run1.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/japanese-typst/old.typ /tmp/old_run2.pdf

# 2. Render new.typ twice to temporary files
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/japanese-typst/new.typ /tmp/new_run1.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/japanese-typst/new.typ /tmp/new_run2.pdf

# 3. Verify byte-for-byte reproducibility across runs and match vendored PDFs
cmp /tmp/old_run1.pdf /tmp/old_run2.pdf
cmp /tmp/old_run1.pdf fixtures/external/japanese-typst/old.pdf
cmp /tmp/new_run1.pdf /tmp/new_run2.pdf
cmp /tmp/new_run1.pdf fixtures/external/japanese-typst/new.pdf

# 4. Clean up temporary files
rm /tmp/old_run1.pdf /tmp/old_run2.pdf /tmp/new_run1.pdf /tmp/new_run2.pdf
```

## Checksums (SHA-256)

### Fixture Files

| File | Size (bytes) | SHA-256 Checksum |
|---|---|---|
| `old.typ` | 411 | `f1389c6a1ce9258bccc406b016b6401277ae09ec7ae6b5387b44b9689fc7680f` |
| `new.typ` | 411 | `8e58f57013857b14e7a29e0782551d2b62d649d17535343a6271ab838f5dfd0f` |
| `old.pdf` | 20003 | `bdfce6e3a69dd7295ad33f91068f5c1aa22a3294146c7bc1f05972b4d101703b` |
| `new.pdf` | 20031 | `9c10f0388ff756ca3e60a69758bf42528ff7039da0c0e0d98423ce86f1e131c5` |

### Upstream Font Files

| Font File | Size (bytes) | SHA-256 Checksum | Role |
|---|---|---|---|
| `NotoSansCJK-Regular.ttc` | 19484784 | `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a` | Body text |
| `NotoSansCJK-Bold.ttc` | 20050760 | `faa5f3656a78b2e2d450d27fe8382c778bc2b6bb5ea29c986664a6a435056ceb` | Heading text |

## Expected Outcome

- **Process Exit Status**: `1` (`ExitStatus::ContentChanges`)
- **Content Changes**: Exactly `1` `Replacement` (`第10版` -> `第20版`, character change `'1'` -> `'2'`)
- **Formatting Changes**: `0`
- **Uncertain Changes**: `0`
- **Unresolved Regions**: `0`
- **Extraction Complete**: `true` on both `old` and `new`
- **Extracted Glyphs**: Exactly 87 glyphs per side, all horizontal direction `(1.0, 0.0)`, all mapped Unicode scalars
- **Comparison Complete**: `true`
- **Alignment Coverage**: `1.0` (100.0%) on both sides
