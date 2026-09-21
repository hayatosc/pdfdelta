# Does a review packet actually cost fewer tokens?

Measured on the registered real-world corpus rather than on synthetic input.

Two answers, because there are two questions. **Reading what the engine settled
costs about a quarter of the document — a 75% reduction at the median.**
**Reading every case costs about 1.6 times the document, down from 2.6.** The
second number is the honest one about a run that settled almost nothing, and
the work recorded here is what brought it down.

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
  8 KiB budget, then `review show` with a 16 KiB budget. Three stopping rules
  are measured:

  - **Settled review** — read index pages while they still hold cases the
    engine settled something about, then open exactly those. The listing is
    ordered by that finding, so everything after the first unsettled page is
    unsettled too.
  - **Full review** — every index page, then every case at the text level.
  - **Exhaustive review** — every index page, then every case at the level its
    own index record names. For material no comparison examined that is the
    quote, so this rule reads the whole document through the packet and is the
    upper bound on what a bundle can cost.

Reproduce with `capture-bundles.py`, `measure-review-rules.py`,
`measure-tokens.py` and `case-composition.py` beside this file.

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

Ratio of packet tokens to full-text tokens, lower is better. `unex` counts the
cases that are material no comparison examined. The superseded column is the
same scripts and budgets against the build before unexamined material was
located rather than quoted.

| Pair | Full text | Cases | unex | Settled | ÷ | Full review | ÷ | was ÷ | Exhaustive ÷ |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| oecd-corporate-governance | 56,334 | 155 | 107 | 19,353 | 0.34 | 66,800 | 1.19 | 2.42 | 2.18 |
| qgis-doc-guidelines-es | 47,211 | 177 | 92 | 11,813 | 0.25 | 79,755 | 1.69 | 2.33 | 2.01 |
| nist-csf-v1-1-to-v2-0 | 44,883 | 98 | 87 | 2,055 | 0.05 | 41,054 | 0.92 | 2.05 | 1.87 |
| w3c-ws-policy-attach | 44,041 | 149 | 77 | 20,124 | 0.46 | 68,763 | 1.56 | 2.55 | 2.26 |
| oasis-odf-packages | 33,062 | 104 | 71 | 13,143 | 0.40 | 46,004 | 1.39 | 2.45 | 2.18 |
| ecma-109-ed10-to-ed11 | 26,853 | 120 | 59 | 5,889 | 0.22 | 51,984 | 1.94 | 2.70 | 2.31 |
| kicad-getting-started | 23,251 | 87 | 87 | 2,004 | 0.09 | 32,970 | 1.42 | 3.04 | 2.70 |
| irs-w4-korean | 20,181 | 10 | 8 | 957 | 0.05 | 9,741 | 0.48 | 0.81 | 0.77 |
| arxiv-attention-v6-to-v7 | 19,784 | 105 | 24 | 2,084 | 0.10 | 46,222 | 2.34 | 3.13 | 2.66 |
| hmrc-sa100 | 8,059 | 37 | 16 | 2,039 | 0.25 | 15,562 | 1.93 | 2.62 | 2.22 |
| irs-form-1040 | 4,248 | 36 | 4 | 2,056 | 0.48 | 15,333 | 3.61 | 4.61 | 3.91 |

- **Settled review: median 0.25 — a 75% reduction, worst case 0.48.** No pair
  costs more than its document. Where the engine settled nothing, the whole
  review is one index page of about 2,000 tokens saying so.
- **Full review: median 1.56, down from 2.55.** Two pairs are now cheaper than
  their own documents; none was before except the one that fits on a single
  index page.
- **Exhaustive review: median 2.22.** Reading the entire document through the
  packet costs more than the document, and always will: the difference is the
  account of what was compared, which the raw text does not carry.

## Where the reduction came from

`oecd` at the text level, all 155 cases, before and after. Field figures count
the encoded values only, so they do not sum to the total, which also carries
each response's keys and punctuation.

| | Before | After |
| --- | ---: | ---: |
| Cases the engine settled a difference in | 23 | 23 |
| Cases that are material nothing examined | 132 (as `not_established`) | 107 (as `not_examined`) |
| `old_text` + `new_text` | 61,726 | 15,903 |
| `available_actions` | 14,791 | 6,726 |
| `evidence` | 13,673 | 3,730 |
| `engine` | 6,975 | 0 |
| **Tokens to read every case** | **121,057** | **52,740** |

The engine's finding used to collapse two different answers into
`not_established`: a comparison that ran and could not conclude, and material
no comparison ever reached. They are worth different amounts of reading, and
separating them is what made the rest possible.

Four changes, largest first:

1. **Unexamined material is located, not quoted.** Its text was the document
   again, re-served one page at a time to a reviewer who already has the
   document. A text answer now carries the interval, its length, and the
   `--detail quote` action that returns it. Quoting is still cheaper for short
   material, so the two encodings are compared and the smaller is served: a
   four-scalar run is quoted, not located.
2. **References for located material are counted, not sampled.** Sixteen of
   several thousand glyph identifiers for a page nothing examined name no
   evidence a reviewer can act on. The full set stays in the bundle, where the
   coverage accounting needs it, and the quote serves it.
3. **A case answer stops repeating its own identifier.** Every entry in
   `available_actions` named the case the answer is already about, four or five
   times per answer. Two retrievals that could only answer "nothing" are no
   longer offered at all: `--detail context` for a case that gathered none, and
   `--detail alternatives` for material no search ever ran over.
4. **The run's outcome moved to the listing.** Six numbers about the whole run
   were repeated in every one of 155 case answers. The listing carries them,
   and a review reads the listing first.

The listing changed too: a record for unexamined material carries how many
scalars it covers and names `quote` as the retrieval worth making, so a
reviewer can weigh a page without opening it.

## Why the full review is still above the document

Not every pair falls below 1.0, and on this corpus most cannot. Every measured
pair exited 3 with no complete comparison, so most cases are genuine open
questions rather than unexamined pages. On `arxiv` 81 of 105 cases were
compared and could not be concluded; reading all of them is reading the
unsettled document plus the account of why it is unsettled. Per case that
account is now about 360 tokens, of which the quoted text is 67: the rest is
where the material is, why the comparison stopped, four completeness
observations, and the references the case owns.

The ratio therefore tracks how much the engine settled, not how well the packet
is encoded. `nist-csf`, where 87 of 98 cases are unexamined pages, reads
everything for 0.92 of its document. `irs-form-1040`, four pages with 36 open
questions, costs 3.61. Both are the same encoding.

## What the comparison is, and is not

The settled review finds every difference the engine could establish, and says
how much it never examined. It does not find differences hiding in the pages it
never examined — on `oecd`, 107 of 155 cases are exactly that, and the listing
now carries their size so a reviewer can see what they are declining to read.

Pasting both documents' text gives a model everything, including those pages,
but hands it 325,000 characters with no structure, no account of what was
compared, and no way to tell a verified equality from an unread page. The two
are not the same review at a different price. Which is worth more depends on
whether an honest account of the engine's blind spots is useful to the caller,
and this measurement cannot settle that.

## Earlier measurements this supersedes

- The first run gave a median ratio of **1.111** — the packet path cost *more*
  than the full text. One answer showed why: 75% of it was a list of 935 glyph
  identifiers, against 16% for the text the reviewer reads. A text answer now
  carries at most sixteen references and declares how many it left out.
- Reading the first ten cases regardless of what they held gave a median of
  0.249 — a similar number to the settled review, reached for the wrong reason.
  Nothing in the packet justified stopping at ten. On `irs-form-1040` it cost
  1.67× the document, because ten arbitrary cases on a four-page form is most
  of the form.

## What this does not show

- Nothing here measures review *quality*. It measures what a reviewer is charged
  to read, and a cheap review is not a better one. Two records beside this file
  take up what these numbers do not: `agent-loop-quality.md`, where one agent
  read one bundle end to end, and `head-to-head.md`, which compares a settled
  review with reading both documents and finds the settled review behind on
  both total findings and findings per token. Independent annotation against
  these pairs has still not been done.
- No pair completed, so there is no measurement of the packet path on documents
  the engine resolves well — the case where the index alone would be the whole
  review, and the case where the full review would be cheapest.
- Seven pairs timed out and two failed. Their costs are unknown, not zero, and
  the timeouts concentrate in the large documents where the saving would be
  largest.
- `cl100k_base` is not the tokenizer of any host that would consume these
  packets. Absolute token figures will differ; the ratios are what this supports.
