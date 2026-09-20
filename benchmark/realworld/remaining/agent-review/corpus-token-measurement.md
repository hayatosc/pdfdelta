# Does a review packet actually cost fewer tokens?

Measured on the registered real-world corpus rather than on synthetic input.
The short answer: **yes — about 74% at the median, and never more than the
document, once the packets say which cases the engine actually settled.** Two
fixes this measurement forced were needed to get there.

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
  Three stopping rules are measured:

  - **Settled review** — read index pages while they still hold cases the
    engine settled something about, then open exactly those. The listing is
    ordered by that finding, so everything after the first unsettled page is
    unsettled too. This is the rule the packets justify.
  - **First ten cases** — an earlier measurement, kept for comparison. Nothing
    in the packet justified stopping at ten; the number is arbitrary.
  - **Full review** — every listing page plus every case.

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

Ratio of packet tokens to full-text tokens, lower is better. `settled` counts
the cases where the engine established a difference; the rest of each bundle's
cases are material it never examined, which the index reports as a count rather
than asking a reviewer to read.

| Pair | Full text | Cases | Settled | Settled review | ÷ full |
| --- | ---: | ---: | ---: | ---: | ---: |
| oecd-corporate-governance | 56,334 | 155 | 23 | 21,474 | 0.38 |
| qgis-doc-guidelines-es | 47,211 | 177 | 11 | 12,880 | 0.27 |
| nist-csf-v1-1-to-v2-0 | 44,883 | 98 | 0 | 2,111 | 0.05 |
| w3c-ws-policy-attach | 44,041 | 149 | 26 | 22,538 | 0.51 |
| oasis-odf-packages | 33,062 | 104 | 17 | 14,704 | 0.44 |
| ecma-109-ed10-to-ed11 | 26,853 | 120 | 4 | 6,273 | 0.23 |
| kicad-getting-started | 23,251 | 87 | 0 | 2,090 | 0.09 |
| irs-w4-korean | 20,181 | 10 | 0 | 1,133 | 0.06 |
| arxiv-attention-v6-to-v7 | 19,784 | 105 | 0 | 2,086 | 0.11 |
| hmrc-sa100 | 8,059 | 37 | 0 | 2,106 | 0.26 |
| irs-form-1040 | 4,248 | 36 | 0 | 2,090 | 0.49 |

**Median 0.261 — a 73.9% reduction, with a worst case of 0.51.** No pair costs
more than its document. Where the engine settled nothing, the whole review is
one index page of about 2,000 tokens saying so.

### The earlier, unjustified cut

Reading the first ten cases regardless of what they held gave a median of 0.249
— a similar number reached for the wrong reason. On `irs-form-1040` it cost
1.67× the document, because ten arbitrary cases on a four-page form is most of
the form. The figures below are kept to show the difference between a rule and
a coincidence.

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

- **Full review: 2.55× the full text at the median.** Reading every case costs
  more than handing over both documents, and no ordering changes that. The
  packets are an index over open questions, not a compression of the document.
- Seeing whether there is anything to review at all — one `review list` — costs
  about 2,000 tokens regardless of document size.

## The two fixes this measurement forced

**The response spent its budget on identifiers.** The first run gave a median
ratio of **1.111** — the packet path cost *more* than the full text. One answer
showed why: 75% of it was a list of 935 glyph identifiers, against 16% for the
text the reviewer reads. A text answer now carries at most sixteen references
and declares how many it left out; the full set stays in the bundle, where the
accounting needs it, and a decision still cites references the stored case
holds. One `oecd` answer fell from 17,455 to 4,437 bytes.

**The packets withheld the engine's own finding.** Every case looked alike, so
there was no ground for stopping anywhere, and reviewing all of them costs 2.55×
the document. Yet the engine had already established a difference in 23 of
`oecd`'s 155 cases and in none of `nist-csf`'s 98 — it simply was not written
down. Cases now carry that finding, the listing is ordered by it, and a reviewer
can stop where the engine stopped instead of at an arbitrary count.

Neither target was met by the design as written. Both were met after measuring
the design against real documents.

## What the comparison is, and is not

The settled review finds every difference the engine could establish, and says
how much it never examined. It does not find differences hiding in the pages it
never examined — on `oecd`, 132 of 155 cases are exactly that.

Pasting both documents' text gives a model everything, including those pages,
but hands it 325,000 characters with no structure, no account of what was
compared, and no way to tell a verified equality from an unread page. The two
are not the same review at a different price. Which is worth more depends on
whether an honest account of the engine's blind spots is useful to the caller,
and this measurement cannot settle that.

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
