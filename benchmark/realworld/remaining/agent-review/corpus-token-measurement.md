# Does a review packet actually cost fewer tokens?

Measured on the registered real-world corpus rather than on synthetic input.
The short answer: **yes for triage, by about 75% at the median — but only after
a fix this measurement forced, and no for reviewing every case.**

## How it was measured

- Corpus: `benchmark/realworld/manifest.tsv`, downloaded with
  `benchmark/realworld/fetch.sh`, which verifies each file's byte count and
  SHA-256 before use. 25 of 29 pairs verified; four could not be retrieved from
  this environment (`edpb-right-of-access-v1-to-final`,
  `bis-operational-risk-2011-to-2021`, `bis-core-principles-2012-to-2024`,
  `khronos-data-format-1-3-to-1-4`).
- Route: `--channels text` with each pair's registered `limit_scale_hint`, and
  the registered 180-second capture timeout.
- Tokenizer: `tiktoken 0.14.0`, encoding `cl100k_base`. This is a named
  third-party instrument applied to both sides of every comparison over the
  same extraction. It is an estimate, it is not any host's reported usage, and
  it is not Claude's tokenizer. What it supports is the ratio between two ways
  of reviewing one document, not an absolute cost.
- Baseline: both documents' extracted glyph text, written by
  `pdfbench audit-agent-review --baseline-text`, so the baseline and the packets
  come from one extractor.
- Packet path: the actual commands, not a model of them. `review list` with an
  8 KiB budget, then `review show --detail text` with a 16 KiB budget.
  **Triage** is the first listing plus the first ten cases. **Full review** is
  every listing page plus every case.

Reproduce with `capture-bundles.py`, `measure-tokens.py` and
`case-composition.py` beside this file.

## What the corpus did

Of 25 pairs: 16 produced a bundle, 7 exceeded the 180-second timeout, 2 ended in
an execution error. **None produced a complete comparison.** Every measurable
pair exited 3, so the packets carry open questions rather than a short list of
residual ones. That is the state the reported "0 of 36" describes, now
reproduced here on the pairs that could be retrieved.

The 16 pairs split into two populations, and conflating them would make the
numbers meaningless:

- **Eleven triage pairs** — text was acquired and most cases can be decided from
  the quoted text. This is the layer the reduction target was stated for.
- **Five acquisition-failure pairs** (`gcc`, `unicode-standard`, `postgresql`,
  `qgis-pyqgis-ja`, `nasa-systems-engineering-handbook`) — the bounded native
  worker acquired nothing usable, so nearly every case is
  `interpret_visual_region` asking for a picture. Their packets are cheap
  because they are empty, not because anything was triaged; `gcc` and
  `postgresql` lost their glyph evidence to a worker crash (`SIGABRT`).

## Result

Ratio of packet tokens to full-text tokens, lower is better.

| Pair | Full text | Triage (10 cases) | ÷ full | Full review | ÷ full |
| --- | ---: | ---: | ---: | ---: | ---: |
| oecd-corporate-governance | 56,334 | 8,869 | 0.16 | 136,106 | 2.42 |
| w3c-ws-policy-attach | 44,041 | 7,691 | 0.17 | 112,317 | 2.55 |
| qgis-doc-guidelines-es | 47,211 | 9,995 | 0.21 | 109,903 | 2.33 |
| nist-csf-v1-1-to-v2-0 | 44,882 | 9,324 | 0.21 | 92,187 | 2.05 |
| oasis-odf-packages | 33,062 | 8,237 | 0.25 | 81,047 | 2.45 |
| ecma-109-ed10-to-ed11 | 26,852 | 6,690 | 0.25 | 72,386 | 2.70 |
| arxiv-attention-v6-to-v7 | 19,784 | 8,720 | 0.44 | 61,873 | 3.13 |
| kicad-getting-started | 23,251 | 11,659 | 0.50 | 70,584 | 3.04 |
| hmrc-sa100 | 8,059 | 6,027 | 0.75 | 21,079 | 2.62 |
| irs-w4-korean | 20,181 | 16,397 | 0.81 | 16,397 | 0.81 |
| irs-form-1040 | 4,247 | 7,088 | 1.67 | 19,588 | 4.61 |

- **Triage: 0.249 at the median — a 75.1% reduction.** The stated target was
  70%, so it is met on this layer.
- **Full review: 2.55× the full text at the median.** Reading every case costs
  more than handing over both documents. The packet path is a way to start and
  to stop honestly, not a cheaper way to read everything.
- Seeing whether there is anything to review at all — one `review list` — costs
  about 2,000 tokens regardless of document size.
- The saving grows with the document. On `irs-form-1040`, 4,247 tokens of text
  in total, triage costs 1.67× the document; pasting it is the right move.

## The fix this measurement forced

The first run of this measurement gave a median triage ratio of **1.111** — the
packet path cost *more* than the full text. Inspecting one answer showed why:
**75% of it was a list of 935 glyph identifiers**, against 16% for the text the
reviewer actually reads. The response layer was spending its budget on its least
decision-relevant field.

A text answer now carries at most sixteen references and declares how many it
left out; the full set stays in the bundle, where the accounting needs it, and a
decision still cites references the stored case holds. One `oecd` answer fell
from 17,455 to 4,437 bytes, and the median triage ratio from 1.111 to 0.249.

The target was not met by the design as written. It was met after measuring the
design against real documents.

## What this does not show

- Nothing here measures review *quality*. It measures what a reviewer is charged
  to read. Whether the packets let an agent reach correct conclusions needs
  human annotation against these same pairs, which has not been done.
- No pair completed, so there is no measurement of the packet path on documents
  the engine resolves well — the case where the index alone would be the whole
  review.
- Seven pairs timed out and two failed. Their costs are unknown, not zero, and
  the timeouts concentrate in the large documents where the saving would be
  largest.
- `cl100k_base` is not the tokenizer of any host that would consume these
  packets. Absolute token figures will differ; the ratios are what this supports.
