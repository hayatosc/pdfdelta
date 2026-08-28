# External Fixture: Text Replacement PDF (Typst)

## Overview

This directory vendors an externally rendered PDF revision pair evaluating text replacement (`Release 10` -> `Release 20` numeric replacement). It provides an independent, real-world compiler dialect (Typst) without requiring external rendering tools at test runtime. `document.yaml` maps the title, first body line, and remaining paragraphs to the canonical benchmark structure; the mutation evaluator verifies that mapping against the rendered PDFs.

## Provenance

- **Renderer**: `typst 0.15.1 (9dfd3a08)`
- **Font & Timestamp Policy**: Built-in default font embedding (Libertinus Serif subsets, OFL-1.1; see [`THIRD-PARTY-NOTICES.md`](../../../THIRD-PARTY-NOTICES.md)) with reproducible timestamp policy (`--creation-timestamp 0 --ignore-system-fonts`).
- **Target Evaluation Case**: Numeric text replacement (`Release 10` -> `Release 20`, 1 replacement).
- **Redistribution**: Safe public domain synthetic test text; contains no personal data, machine-specific paths, or secrets.

## Generation Commands

```bash
# Generated with typst 0.15.1
typst compile --creation-timestamp 0 --ignore-system-fonts old.typ old.pdf
typst compile --creation-timestamp 0 --ignore-system-fonts new.typ new.pdf

# From the repository root, evaluate the vendored outputs against the canonical mutation.
mise run bench-evaluate-rendered-yaml -- \
  fixtures/external/case3-typst/document.yaml \
  --old-pdf fixtures/external/case3-typst/old.pdf \
  --new-pdf fixtures/external/case3-typst/new.pdf \
  --renderer typst-0.15.1 \
  number-replace --paragraph-id release --new-number 20
```

## Checksums (SHA-256)

| File | Size (bytes) | SHA-256 Checksum |
|---|---|---|
| `document.yaml` | 392 | `be2bd83e417227b95532e99936558ef73717706e435e50e7ff4b0450abaebe8d` |
| `old.typ` | 338 | `34159bcfda48edba6e56f4c765895a181600374de6ba7204dcd67f8450da5058` |
| `new.typ` | 338 | `b99dac2114a9da434b3718e39cd8a8fd66f9f86db60dc1c187fd6dc588fb7869` |
| `old.pdf` | 12688 | `7b484999907840add3056281c55ef74090a061c56fdbd2017739a35b4b4dc6de` |
| `new.pdf` | 12743 | `2af5047c7ae3fc7c7357ca0f0447bff6493bc5e12e00437c621ed4987edb8711` |

## Expected Outcome

- **Comparison CLI Exit Status**: `1` (`ExitStatus::ContentChanges`)
- **Benchmark CLI Exit Status**: `0` (the expected replacement was recovered)
- **Content Changes**: Exactly `1` `Replacement` (`Release 10` -> `Release 20`, character change `'1'` -> `'2'`)
- **Formatting Changes**: `0`
- **Uncertain Changes**: `0`
- **Unresolved Regions**: `0`
- **Extraction Complete**: `true` on both `old` and `new`
- **Comparison Complete**: `true`
- **Alignment Coverage**: `1.0` (100.0%) on both sides
