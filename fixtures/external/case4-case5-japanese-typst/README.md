# External Fixture: Japanese Paragraph Insertion and Deletion PDF (Typst)

## Overview

This directory vendors an externally rendered Japanese PDF revision pair evaluating paragraph insertion producing 1 exact change in forward comparison and paragraph deletion producing 1 exact change in reverse comparison (born-digital horizontal Japanese text with single-column layout). It provides an independent, real-world compiler dialect (Typst) and CJK font embedding (Noto Sans CJK JP subsets) without requiring external rendering tools at test runtime.

The two documents share identical page geometry, margins (2.5cm), font (`Noto Sans CJK JP`, 11pt), paragraph spacing (24pt), heading, and surrounding body paragraphs:
- `old.typ` contains 3 blocks (1 heading + 2 paragraphs, 66 glyphs).
- `new.typ` inserts exactly one complete, semantically distinct paragraph ("運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。", 34 glyphs) at block index 2, totaling 4 blocks (100 glyphs).

## Provenance & Environment Assumptions

- **Renderer**: `typst 0.15.1 (9dfd3a08)` (see [`fixtures/external/japanese-typst/README.md`](../japanese-typst/README.md) for toolchain details).
- **Font Package**: Debian/Ubuntu package `fonts-noto-cjk` version `1:20230817+repack1-3` (upstream release tag `Sans2.004`; note: font path `/usr/share/fonts/opentype/noto` is Debian/Ubuntu-specific).
- **Embedded Font Files Used by Typst**:
  - `/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc` (size: 19,484,784 bytes, SHA-256: `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a`) — used for body paragraphs.
  - `/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc` (size: 20,050,760 bytes, SHA-256: `faa5f3656a78b2e2d450d27fe8382c778bc2b6bb5ea29c986664a6a435056ceb`) — used for headings.
- **Font Copyright & License**: SIL Open Font License, Version 1.1 (see [`THIRD-PARTY-NOTICES.md`](../../../THIRD-PARTY-NOTICES.md)).
- **Target Evaluation Cases**: Paragraph insertion and paragraph deletion in Japanese.
- **Redistribution**: Safe public domain synthetic test text; contains no personal data, machine-specific paths, or secrets.

## Repo-Root-Complete Generation Commands

```bash
# From repository root (assumes typst 0.15.1 and Debian/Ubuntu fonts-noto-cjk package installed at /usr/share/fonts/opentype/noto)
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case4-case5-japanese-typst/old.typ fixtures/external/case4-case5-japanese-typst/old.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case4-case5-japanese-typst/new.typ fixtures/external/case4-case5-japanese-typst/new.pdf
```

## Deterministic Verification Recipe

Run this recipe from the repository root to verify byte-for-byte reproducibility:

```bash
# 1. Render old.typ twice to temporary files
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case4-case5-japanese-typst/old.typ /tmp/old_run1.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case4-case5-japanese-typst/old.typ /tmp/old_run2.pdf

# 2. Render new.typ twice to temporary files
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case4-case5-japanese-typst/new.typ /tmp/new_run1.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts --font-path /usr/share/fonts/opentype/noto fixtures/external/case4-case5-japanese-typst/new.typ /tmp/new_run2.pdf

# 3. Verify byte-for-byte reproducibility across runs and match vendored PDFs
cmp /tmp/old_run1.pdf /tmp/old_run2.pdf
cmp /tmp/old_run1.pdf fixtures/external/case4-case5-japanese-typst/old.pdf
cmp /tmp/new_run1.pdf /tmp/new_run2.pdf
cmp /tmp/new_run1.pdf fixtures/external/case4-case5-japanese-typst/new.pdf

# 4. Clean up temporary files
rm /tmp/old_run1.pdf /tmp/old_run2.pdf /tmp/new_run1.pdf /tmp/new_run2.pdf
```

## Checksums (SHA-256)

### Fixture Files

| File | Size (bytes) | SHA-256 Checksum |
|---|---|---|
| `old.typ` | 350 | `5ab0e0359b55e8877421670f013d2289aedca219689cf3588a78830052dcf352` |
| `new.typ` | 454 | `8aa90b3bc90c364e0409e2b1ff0cc8b025bb8fce6d50394cfadc369d61d3655a` |
| `old.pdf` | 17086 | `631df7e57f375e694fe27908363ad50afa96b0939a053532d629a6de9d25bdb4` |
| `new.pdf` | 21701 | `fdd1af7efbaaf43961b655faecc71a935081884cf751ac9e9fd5581a989eda43` |

### Upstream Font Files

| Font File | Size (bytes) | SHA-256 Checksum | Role |
|---|---|---|---|
| `NotoSansCJK-Regular.ttc` | 19484784 | `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a` | Body text |
| `NotoSansCJK-Bold.ttc` | 20050760 | `faa5f3656a78b2e2d450d27fe8382c778bc2b6bb5ea29c986664a6a435056ceb` | Heading text |

## Expected Outcome

### Forward Comparison (Case 4: `old.pdf` -> `new.pdf`)
- **Process Exit Status**: `1` (`ExitStatus::Changed`) in both default and `--strict` modes
- **Content Changes**: Exactly `1` (`kind: "insertion"`, text: `"運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。"`, block: `2`, page: `0`)
- **Formatting Changes**: `0`
- **Uncertain Changes**: `0`
- **Unresolved Regions**: `0`
- **Extraction Complete**: `true` on both sides
- **Alignment Coverage**: `1.0` (66/66 old, 100/100 new)

### Reverse Comparison (Case 5: `new.pdf` -> `old.pdf`)
- **Process Exit Status**: `1` (`ExitStatus::Changed`) in both default and `--strict` modes
- **Content Changes**: Exactly `1` (`kind: "deletion"`, text: `"運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。"`, block: `2`, page: `0`)
- **Formatting Changes**: `0`
- **Uncertain Changes**: `0`
- **Unresolved Regions**: `0`
- **Extraction Complete**: `true` on both sides
- **Alignment Coverage**: `1.0` (100/100 old, 66/66 new)
