# External Fixture: Japanese Line-Wrap-Only PDF (Typst)

## Overview

This directory vendors an externally rendered Japanese PDF revision pair evaluating line-wrap-only changes producing zero content changes (born-digital horizontal Japanese text with single-column layout). It provides an independent, real-world compiler dialect (Typst) and CJK font embedding (Noto Sans CJK JP subsets) without requiring external rendering tools at test runtime.

The text payload between `old.typ` and `new.typ` is 100% byte-identical; only page margin declarations differ (`margin: 2.5cm` vs `margin: 4.5cm`), causing the body paragraphs to wrap across materially different line boundaries.

## Provenance & Environment Assumptions

- **Renderer**: `typst 0.15.1 (9dfd3a08)` (see [`fixtures/external/japanese-typst/README.md`](../japanese-typst/README.md) for toolchain details).
- **Font Package**: Debian/Ubuntu package `fonts-noto-cjk` version `1:20230817+repack1-3` (upstream release tag `Sans2.004`; note: font path `/usr/share/fonts/opentype/noto` is Debian/Ubuntu-specific).
- **Embedded Font Files Used by Typst**:
  - `/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc` (size: 19,484,784 bytes, SHA-256: `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a`) — used for body paragraphs.
  - `/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc` (size: 20,050,760 bytes, SHA-256: `faa5f3656a78b2e2d450d27fe8382c778bc2b6bb5ea29c986664a6a435056ceb`) — used for headings.
- **Font Copyright & License**: SIL Open Font License, Version 1.1 (see [`THIRD-PARTY-NOTICES.md`](../../../THIRD-PARTY-NOTICES.md)).
- **Target Evaluation Case**: Line-wrap-only change with 0 content changes in Japanese.
- **Redistribution**: Safe public domain synthetic test text; contains no personal data, machine-specific paths, or secrets.

## Repo-Root-Complete Generation Commands

```bash
# From repository root (assumes typst 0.15.1 and Debian/Ubuntu fonts-noto-cjk package installed at /usr/share/fonts/opentype/noto)
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case1-japanese-typst/old.typ fixtures/external/case1-japanese-typst/old.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case1-japanese-typst/new.typ fixtures/external/case1-japanese-typst/new.pdf
```

## Deterministic Verification Recipe

Run this recipe from the repository root to verify byte-for-byte reproducibility:

```bash
# 1. Render old.typ twice to temporary files
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case1-japanese-typst/old.typ /tmp/old_run1.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case1-japanese-typst/old.typ /tmp/old_run2.pdf

# 2. Render new.typ twice to temporary files
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case1-japanese-typst/new.typ /tmp/new_run1.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case1-japanese-typst/new.typ /tmp/new_run2.pdf

# 3. Verify byte-for-byte reproducibility across runs and match vendored PDFs
cmp /tmp/old_run1.pdf /tmp/old_run2.pdf
cmp /tmp/old_run1.pdf fixtures/external/case1-japanese-typst/old.pdf
cmp /tmp/new_run1.pdf /tmp/new_run2.pdf
cmp /tmp/new_run1.pdf fixtures/external/case1-japanese-typst/new.pdf

# 4. Clean up temporary files
rm /tmp/old_run1.pdf /tmp/old_run2.pdf /tmp/new_run1.pdf /tmp/new_run2.pdf
```

## Checksums (SHA-256)

### Fixture Files

| File | Size (bytes) | SHA-256 Checksum |
|---|---|---|
| `old.typ` | 628 | `732d8d8cc4627a8bcf9ac27dd4635fb7902db4ad8654d6079722aaa51d4b1698` |
| `new.typ` | 628 | `0e4cb5067016000b905134bbb5be67d19decfe4b9c87fee07d9ff0bb67ca26d7` |
| `old.pdf` | 26681 | `47fd47f6dd6a1316612880a76af15b4dfec121b182dbde789578b17e1e3c35ab` |
| `new.pdf` | 26651 | `f0016e01593c1b4c994e1e94c453d80c519828c0c7b6bd798ccc32184a38d559` |

### Upstream Font Files

| Font File | Size (bytes) | SHA-256 Checksum | Role |
|---|---|---|---|
| `NotoSansCJK-Regular.ttc` | 19484784 | `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a` | Body text |
| `NotoSansCJK-Bold.ttc` | 20050760 | `faa5f3656a78b2e2d450d27fe8382c778bc2b6bb5ea29c986664a6a435056ceb` | Heading text |

## Expected Outcome

- **Process Exit Status**: `0` (`ExitStatus::Unchanged`) in both default and `--strict` modes
- **Content Changes**: Exactly `0`
- **Formatting Changes**: `2` (`normalization` from line-wrap position shifts in multi-line paragraphs)
- **Uncertain Changes**: `0`
- **Unresolved Regions**: `0`
- **Extraction Complete**: `true` on both `old` and `new`
- **Extracted Glyphs**: Exactly 158 glyphs per side, all horizontal direction `(1.0, 0.0)`, all mapped Unicode scalars
- **Reconstructed Lines**: 6 lines in `old` vs 7 lines in `new`
- **Comparison Complete**: `true`
- **Alignment Coverage**: `1.0` (100.0%) on both sides
