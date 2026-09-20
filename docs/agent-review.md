# Agent review packets

pdfdelta reports what it can establish from PDF evidence and retains what it
cannot. Agent review packets are the interface for the second part: they turn
the comparison's remaining obligations into individual decisions, each carrying
the minimum evidence that decision needs, so that a calling agent can retrieve
more only where it actually helps.

pdfdelta does not run an agent. It writes a local bundle and answers bounded
queries against it. There is no external analysis API, no API key, no model
download, and no resident service.

This document describes the contract and the commands that exist today.
Retrieval is added incrementally; what is not implemented yet is named as such
rather than left to be discovered.

## What a packet is, and what it is not

A packet is a review note's input, never a proof.

- The engine's comparison is unchanged by export. Selected channels, coverage,
  strict source masks, change events, and the process exit status are identical
  with and without a packet.
- An external reviewer's answer is stored beside the comparison, never merged
  into it. No agent selection, confidence, or image reading promotes a
  non-owning or inferred observation to a strict result.
- Omission is not resolution. Output budgets, search budgets, candidate
  truncation, and unretrieved images are reported separately from one another.

## The two comparison contracts

Two pipelines produce cases, and a case always records which one it came from.
Their obligations are not interchangeable.

| | Shared evidence pipeline | Native-text adapter |
| --- | --- | --- |
| Selected material | Text, visual, form, and relationship channels selected by the caller | Extracted native glyphs only |
| Discovery | Per-channel inventories with explicit completeness | Extraction outcome for the glyph stream |
| Unresolved obligations | Uncompared source references, unresolved scopes, unresolved relations, evidence issues | Unresolved resolution ranges, tentative relations, competing candidates |
| Correspondence | Proposal generation plus a weighted ownership solver per scope | Anchor verification and localized edit scripts |

A packet produced from the native-text adapter never suggests that visual, form,
or relationship evidence was examined. Its cases carry the text-only contract
explicitly.

## Result classes are preserved

Human review already separates three classes, and packets keep them apart:

- **Strict** — conditional on an accepted correspondence, with exact source
  masks. This is the only class that owns source references.
- **Non-owning range** — a comparison of a corresponding interval that owns no
  sources and discharges no coverage obligation.
- **Inferred** — an inferred correspondence, or an observation that depends on
  one.

An external assessment is a fourth, separate class. It is recorded with its
origin and never counted as any of the three.

## Four kinds of completeness

Every case carries four independent observations. They answer different
questions and are never collapsed into one flag. Each is three-valued:
`complete`, `incomplete`, or `unknown`, because an observation the engine never
made is different from a search that ran and stopped short.

| Observation | Question it answers |
| --- | --- |
| `evidence` | Did discovery close for the evidence this case depends on? |
| `candidate_enumeration` | Was the supplier universe of competing hypotheses closed? |
| `solver_search` | Did the correspondence search finish over those candidates? |
| `response` | Does this response carry everything the case holds locally? |

One returned hypothesis is never uniqueness: a case with a single hypothesis and
an incomplete enumeration is a case whose alternatives were not enumerated.

## Cases, gaps, and material with no candidate

A case is a question to answer, not a difference to approve. Its unit is the
local component that a decision actually needs — the competing correspondences,
the dependency boundary, and the original text — rather than a page number or a
fixed character count.

Unresolved material that cannot be attached to any located case is not dropped.
It becomes an explicit gap at document, page, or channel scope, including:

- pages from which nothing could be extracted,
- inventories that never closed, so the amount of undiscovered evidence is
  unknown,
- channels whose interpretation is not implemented,
- scopes the comparison never visited.

An inventory that did not close has no denominator. The number of discovered
references is never presented as a share of the document.

## Export budgets are not comparison completeness

The export has its own budgets for source visits, candidate visits, case count,
and output size. When a budget stops the export, the affected material is
retained as a gap on its parent scope and the export reports
`export_complete: false`.

That flag is independent of the comparison's own completeness. A fully complete
comparison can produce a truncated export, and a truncated export never makes an
unresolved comparison look resolved.

## Output budgets

A response budget is a hard cap on the whole encoded JSON document, including
metadata, escaped strings, and any continuation cursor. Within that cap:

- required metadata is reserved first, and records fill what remains;
- no string, record, or JSON document is cut in the middle;
- omitted material keeps its reference, its length, and the action that
  retrieves it;
- long text is cut at sentence and structural boundaries rather than at a fixed
  character count, and a case that cannot carry the context needed for a
  decision says so instead of silently shortening it;
- a budget too small to carry even one record is an explicit error that names
  the required budget or a smaller view, never an unchanging cursor.

Byte budgets are not token counts. Any token figure must name the tokenizer and
its version and be labelled an estimate; CJK text, JSON escaping, and images are
measured separately.

## Exit status

The comparison's process status is unchanged by export:

| Code | Meaning |
| --- | --- |
| 0 | Complete comparison, no content changes |
| 1 | Complete comparison, content changes |
| 2 | Execution or output error |
| 3 | Incomplete comparison |

A bundle written during an incomplete comparison is still usable, and the
comparison still exits 3. Callers should treat 0, 1, and 3 as results that carry
a bundle, and 2 as an error. Retrieval commands report the original comparison
status inside their payload rather than through their own exit code.

## Commands

Export a bundle during an ordinary comparison:

```sh
pdfdelta old.pdf new.pdf --channels text --agent-review ./review-run
pdfdelta old.pdf new.pdf --native-text-only --agent-review ./text-review-run
```

The destination must not exist. `--agent-review` and `--review` cannot be used
together: one comparison publishes one bundle. Everything else about the run is
unchanged, including the text report, the JSON report, and the exit status.

Read the bundle back within an explicit budget:

```sh
pdfdelta review list ./review-run --max-output-bytes 8192
pdfdelta review list ./review-run --cursor CURSOR --max-output-bytes 8192
pdfdelta review show ./review-run --case R17 --detail index
pdfdelta review show ./review-run --case R17 --detail text --max-output-bytes 16384
pdfdelta review show ./review-run --case R17 --detail alternatives --cursor CURSOR
```

A query that reads successfully exits 0 whatever the comparison concluded; the
engine's own status travels inside the payload. A query that cannot be answered
exits 2 and prints a JSON refusal naming what went wrong and, for a budget
failure, the number of bytes it would need.

`review` is a subcommand name, so a file actually named `review` must be given
as `./review` to be read as an input path.

Not implemented yet: `--detail context`, `--detail visual`, local region
rendering, and importing an external assessment. The parser rejects the detail
levels it cannot serve instead of accepting them and answering with a stub, and
the manifest's capability list reports exactly what this build answers.

## Bundle layout

```text
review-run/
  old.pdf, new.pdf     the exact acquired bytes the comparison examined
  cases/<case>.json    one packet per case
  cases/index.json     the compact listing `review list` pages through
  manifest.json        written last; its presence means the bundle is complete
```

Files are published atomically into a new directory with owner-only access, the
whole bundle is bounded to 512 MiB, and every artifact is listed in the manifest
with its size and SHA-256. Artifact names are generated by the program; nothing
in a document can choose where bytes land.

A bundle without `manifest.json` is incomplete and must not be read.

Every artifact a query reads is checked against the digest the manifest recorded
when the bundle was published, and a file that no longer matches is refused
rather than answered. This detects a bundle that drifted or was edited after
publication; it is not a defence against replacing the whole bundle, manifest
included.

## Cursors

A cursor is opaque and bound to one bundle identity and one query shape. Using a
cursor against another bundle, or against a different query, is refused rather
than reinterpreted, because the record order differs between them. A listing
that omits records always returns a cursor that advances; a query never returns
the same cursor twice.

## External assessments

An assessment answers one case with a conclusion, a short rationale, references,
and its limitations. Long reasoning traces are not requested and not stored.

Statuses are `changed`, `unchanged_in_scope`, `need_more_evidence`, and
`undetermined`. `unchanged_in_scope` is a statement about the examined range
only. An unanswered or rejected assessment is never completed to "unchanged".

Assessments are validated against the bundle they name: schema, bundle identity,
case identity, hypothesis existence, reference resolution including the
document side, and mutually exclusive selections. Passing validation means the
answer is well formed and references real evidence. It does not mean the answer
is correct.

## Trust boundary

Document text, file names, extracted strings, candidate descriptions, and
external assessments are untrusted data. They are quoted for a reviewer to read
and never become instructions, file names, or command arguments. Artifact names
are generated by the program.

pdfdelta itself sends nothing anywhere. Evidence handed to a host agent is
governed by that host's data handling, so "a local CLI means the document never
reaches a model" is not an accurate description of the boundary. For confidential
documents, the host's permissions and transmission scope are the boundary.
