# Does an agent reading these packets reach correct conclusions?

The cost measurement says what a reviewer is charged to read. It says nothing
about whether the reading works. This is the first attempt at the second
question, on one pair, by one agent.

**Result: every finding the packets reported was real, and the packets reported
about one twentieth of the document's changes.** Precision 23 of 23; recall
about 4%.

## What was done

`oecd-corporate-governance-2015-to-2023`, the corpus pair with the most settled
findings. The reviewer used only the documented commands and budgets — no
access to the PDFs, the report, or the bundle's files — and followed the
settled-review rule:

```sh
pdfdelta review list   ./bundle --max-output-bytes 8192          # 22 of 23 settled cases
pdfdelta review list   ./bundle --cursor … --max-output-bytes 8192
pdfdelta review show   ./bundle --case R… --detail text --max-output-bytes 16384   # x23
pdfdelta review import ./bundle --decisions d.json --output reviewed.json
```

The first listing page held 22 of the 23 `difference_established` cases and the
23rd was the first record of page two, which is the ordering working as
designed. Each case was answered `changed` with a change kind, the references
the answer itself served, and a rationale naming the difference. The import
accepted all 23: `reviewed_cases: 23`, `unanswered_cases: 132`.

Verification then used the baseline text of both documents, written by
`pdfbench audit-agent-review --baseline-text` from the same extractor, which
the reviewer did not see while deciding.

## Precision: 23 of 23

Every case quoted two sides that really differ, and both sides were located in
their own document.

| Check | Result |
| --- | ---: |
| Cases whose quoted old and new text actually differ | 23 / 23 |
| Quoted new text located in the new document | 23 / 23 |
| Quoted old text located in the old document | 23 / 23 |

The old-side check needs one qualification. Seven of the 23 old quotes match the
baseline from their first character; the other sixteen match from a fragment
inside the quote but not from their start, because the case's reconstructed view
reads `D. Shareholders, including…` where the linear glyph dump reads
`D.Shareholders, including…`. That is the two text orders disagreeing about one
reconstructed space, which the case declares as `inferred_reading_order`. It is
not fabricated text: every quote is present in its document.

The findings themselves are ordinary editorial revisions — `the firm` becoming
`the company`, `delegating accountability` becoming `delegating authority`,
`investment relations officer` becoming `investor relations officer`, a
principle gaining `and diversity`, and the renumbering of principles from `D.`
to `II.D.` throughout.

## Recall: about 4%

Sentence-level diff of the two baseline texts, counting only sentences of 40
characters or more so that page furniture and headings do not dominate:

| | |
| --- | ---: |
| Old-side sentences | 591 |
| New-side sentences | 981 |
| Sentences identical and in the same position | 90 |
| Old-side sentences inside a changed region | 501 |
| Of those, quoted by the 23 settled cases | 22 (4.4%) |

The two editions are substantially different documents, so 501 changed regions
is the document's own doing rather than a failure to align. Still, the settled
review reports 23 of them. This is consistent with the engine's own accounting,
which compared 193 of 137,926 old glyphs: the packets are an honest view of a
comparison that examined 0.14% of the material, and the reviewer cannot see
more than the comparison did.

The settled findings are not a random 4%. They concentrate in the numbered
principles, which is where a reader of this document would look first. That is
an observation about one pair, not a demonstrated property.

## What this exercise found in the contract

One defect, now fixed: a decisions file that did not have the required shape was
refused as `malformed_bundle` with the message `data did not match any variant
of untagged enum SubmittedDecisions`. The caller's file is not the bundle, and
the message named no field, so a host could not repair its own submission from
it. Submissions now fail as `malformed_decisions` or `unreadable_decisions`, and
the refusal names the missing field and states the required shape.

Everything else behaved as documented: budgets held, the listing order put the
settled cases first, the cursor advanced, and the import refused nothing it
should have accepted.

## Why this is weak evidence

- **The reviewer evaluated itself.** The same agent read the packets, made the
  decisions and scored them. An independent annotator would be a different
  measurement, and a better one.
- **One pair.** Nothing here generalises to the other fifteen.
- **Precision was checked against the same extractor that produced the
  packets.** It establishes that the packets quote their own documents
  faithfully, not that the extraction is right about the PDFs.
- **Recall depends on the ground truth.** A sentence diff of reflowed extracted
  text is an estimate, not a reference annotation. The 4% figure should be read
  as an order of magnitude.
- **The settled cases are all differences by construction.** Answering them
  `changed` confirms the engine rather than challenging it. A harder test would
  include cases the engine could not settle, where a reviewer has to disagree.
