# Is a review packet worth more to an agent than the document?

The cost measurement said a settled review costs a quarter of the document. It
did not say what the two buy. This record fills in the other half of that
comparison.

**On this corpus, the answer is no.** Reading both documents' text found 4.9
times as many changes for 2.5 times the tokens — about twice the findings per
token. The packet's ceiling is the engine's: a settled review can only report
what the comparison examined, and these comparisons examined about one percent
of their documents.

## How it was run

- Ground truth: sentence-level diff of both baseline texts, sentences of 40
  characters or more, written by `pdfbench audit-agent-review --baseline-text`
  so both arms are scored over one extraction. It is an estimate over reflowed
  text, not a reference annotation.
- The scorer, `score-headtohead.py`, was written and committed to the scratch
  directory **before either arm was run**, so neither arm could be tuned to it.
- Reader arm: `oasis-odf-packages-v1-2-to-v1-3`, chosen because the reader had
  never opened any of its cases and because neither edition contains a change
  log that would hand over the answer. The reader read both baseline texts in
  full and submitted 49 old-side fragments naming the differences it found.
- Packet arm: computed mechanically for every pair. A settled review's recall
  is a property of the bundle — which changed sentences the settled cases quote
  — so it needs no reader and cannot be flattered by one.

## The head-to-head

`oasis-odf-packages-v1-2-to-v1-3`, 120 changed old-side sentences:

| Arm | Tokens | Found | Recall | Recall per 1,000 tokens |
| --- | ---: | ---: | ---: | ---: |
| Settled review | 13,143 | 8 | 6.7% | 0.51% |
| Both documents' text | 33,062 | 39 | 32.5% | 0.98% |

The reader arm wins on both axes. It is not only more complete in absolute
terms, it is more efficient per token, which is the claim the cost measurement
was closest to making and cannot support.

Twelve of the reader's 49 fragments matched no changed sentence in the ground
truth — differences it named that the sentence diff did not classify that way.
If anything the reader's recall is understated.

## The packet arm across the corpus

| Pair | Settled cases | Changed sentences | Found | Recall |
| --- | ---: | ---: | ---: | ---: |
| oasis-odf-packages | 17 | 120 | 8 | 6.7% |
| w3c-ws-policy-attach | 26 | 118 | 6 | 5.1% |
| oecd-corporate-governance | 23 | 501 | 22 | 4.4% |
| ecma-109-ed10-to-ed11 | 4 | 98 | 2 | 2.0% |
| nist-csf-v1-1-to-v2-0 | 0 | 589 | 0 | 0% |
| kicad-getting-started | 0 | 75 | 0 | 0% |
| irs-form-1040 | 0 | 21 | 0 | 0% |
| hmrc-sa100 | 0 | 20 | 0 | 0% |
| irs-w4-korean | 0 | 15 | 0 | 0% |
| nasa-systems-engineering-handbook | 0 | 5,239 | 0 | 0% |
| unicode-standard-15-to-16 | 0 | 11,483 | 0 | 0% |

Best case 6.7%; zero on seven of eleven measurable pairs.

## Why, and what it is not

This is not a verdict on the packet format. On a separate pair every one of 23
settled findings was a real difference, correctly quoted from its own document:
precision was 23 of 23. The packets do not invent, mislocate or overstate.

The ceiling is the comparison behind them. On `oecd` it compared 193 of 137,926
glyphs — 0.14%. `shared-evidence-coverage-diagnosis.json` locates that in the
correspondence solver, which accepted four proposals out of ten thousand and
did not accept more when the budgets were raised sixty-four times. A packet
cannot report material that was never compared, so until that changes, no
encoding of the result will beat reading the document.

One thing the reader arm did not have: it found 39 of 120 changes and had no
way to know it had missed 81. The packet reported 8 and said, per page, how
much it never examined. Whether that account is worth the other 31 findings
depends on what the caller needs, and this measurement does not decide it.

## Limits of this record

- One reader arm, on one pair, by one reader, who also wrote the scorer before
  reading. The pre-registration limits the damage; it does not remove it.
- No corpus pair produced a complete comparison, so nothing here measures the
  case the contract was designed for — an engine that settles most of a
  document and leaves a short list of residual questions.
- The ground truth is a sentence diff, not annotation. Both arms are scored
  against the same estimate, so the comparison holds even where the absolute
  numbers do not.
