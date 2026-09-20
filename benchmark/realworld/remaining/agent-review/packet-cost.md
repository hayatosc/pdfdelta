# What a review packet costs to read

Measured with `pdfbench audit-agent-review`, which runs no model, calls no
external service, and needs no network. Every figure is **bytes**, which are
exact. Tokens are not measured: a token count depends on a tokenizer and its
version, and quoting one without naming them invites comparing figures produced
by different instruments. A host's own usage, when it supplies one, is carried
through in its own section and never mixed with these.

## What is being compared

- **Full text** — both documents' extracted glyph text, as a host would paste
  it into a prompt.
- **Engine report** — the existing version 2 JSON report for the same run.
- **First read** — the case index plus one median case packet: what it costs to
  see every open question and then open one of them.

The first read is the honest unit for a retrieval loop. The bundle's total size
is a disk cost, not an input cost: a query's answer is capped by its own output
budget, and a loop stops once it can decide. The index figure here is the whole
index; a real `review list` returns only what its budget allows, so the first
read in practice is smaller than the number below.

## Measurements

| Input | Comparison | Cases | Full text | Report | Index | Median case | First read vs full text |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `case1-japanese-typst` | complete | 0 | 948 | 24,605 | 62 | — | −93.5% |
| `case2-japanese-typst` | complete | 0 | 600 | 22,617 | 62 | — | −89.7% |
| `case3-typst` | complete | 1 | 444 | 155,135 | 422 | 4,034 | **+903.6%** |
| synthetic 80-page, one changed value | incomplete | 161 | 445,822 | 859,627 | 57,265 | 73,513 | −70.7% |

The synthetic input is 80 pages of 40 lines, identical on both sides except for
one `10 days` → `20 days`. It is generated, not a real publication, and is used
only to measure how the packet path scales; it establishes nothing about real
documents.

## What this shows

The packet path saves input only when there is a document to be saved from.

- When the comparison completes and leaves nothing open, the entire review is
  the index: a few dozen bytes against the whole text. This is the best case and
  it is the common one for small, well-behaved inputs.
- On a one-paragraph document with a single open case, the packet costs about
  ten times the text. There is nothing to triage, and the packet's reasons,
  completeness and evidence references are pure overhead. Handing over the text
  is the right move at that size, and a caller should.
- On the 80-page input the first read is about 29% of the full text even though
  the comparison resolved nothing at all — the saving comes from reading one
  page's worth of material instead of eighty.

The design target of a 70% median reduction remains a target, not a result. It
was stated for documents that are mostly native text with local changes, and the
only input here of that shape is synthetic. Measuring it properly needs the
non-vendored corpus, which is absent from this environment for the same reason
the baseline in `baseline.md` could not be reproduced here.

## What the export costs on disk

For the 80-page input, the bundle is 31 MB: 18.5 MB of page rasters, 11.2 MB of
case packets, 0.4 MB of context, 0.7 MB of source PDFs. Page rasters dominate
because every page a case could ask to see is published. A caller that does not
need pictures pays that cost anyway today; bounding it per bundle rather than
per page is the obvious next measurement, not a change made on the strength of
one synthetic input.

## Reproducing

```sh
pdfdelta OLD NEW --channels text --agent-review ./run -j ./report.json --quiet
pdfbench audit-agent-review --bundle ./run --report ./report.json
```

The audit exits non-zero when any packet invariant is violated, so it can gate a
change to the contract. Findings are invariant violations — a digest that no
longer matches, an index that disagrees with a packet, a case with no reason, a
picture offered for a page that was never published — not judgements about
review quality.
