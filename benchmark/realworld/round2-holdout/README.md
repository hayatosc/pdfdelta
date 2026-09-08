# Round-2 annotated holdout

This directory contains one previously unused public revision pair for an evaluation-only holdout:
Oracle's *Java Language Specification*, Java SE 17 to Java SE 21. It is kept separate from the
original 29-pair corpus and its frozen 39 measurable expectations. It is also separate from the
RFC 8941 to RFC 9651 holdout in [`../claims-holdout.tsv`](../claims-holdout.tsv), which has no
reviewed annotations.

The pair has one partial annotation in [`expected/jls-se17-to-se21.json`](expected/jls-se17-to-se21.json).
The reviewed scope is the definition of record patterns in section 14.30.1. Java SE 17 contains
the type-pattern definition only; Java SE 21 adds this sentence:

> A record pattern is used to test whether a value is an instance of a record class type and, if it is, to recursively perform pattern matching on the record component values.

The expected event is one `insertion`. Its new-side source mask is `[0, 173)`, covering every
character of the inserted sentence after PDF whitespace is collapsed. The sentence occurs once in
the extracted Java SE 21 text. The corresponding sentence occurs zero times in Java SE 17.

The source review used both official PDFs and their extracted text. The scoped sentence is on PDF
page 573 (printed page 557) in Java SE 21; the corresponding Java SE 17 pattern-definition page is
PDF page 560 (printed page 544). After collapsing PDF whitespace, the exact sentence occurs once in
the Java SE 21 extracted text and zero times in the Java SE 17 extracted text. The surrounding type
pattern sentence and section anchor were checked on both pages, so this annotation is based on the
source text rather than release-feature metadata alone. The PDF metadata was also checked: the
first pages identify Java SE 17 and Java SE 21 respectively, with release dates in August 2021 and
August 2023.

The parser's evidence report recorded incomplete extraction for both full documents because of
unsupported non-rectangular clipping paths. The old document reported pages 73, 152, 153, 566,
687-691, 758-760, 766, and 770. The new document reported pages 75, 154, 155, 582, 704-708,
776-778, 784, and 788. The reviewed pattern-definition pages are outside those reported pages, so
the annotation remains usable as a bounded scope while the manifest records `incomplete`
extraction for the pair as a whole.

The PDFs are intentionally not checked in. Reproduce the provenance capture in a temporary cache:

```sh
mkdir -p /tmp/pdfdelta-round2-holdout
curl --fail --location --output /tmp/pdfdelta-round2-holdout/jls17.pdf \
  https://docs.oracle.com/javase/specs/jls/se17/jls17.pdf
curl --fail --location --output /tmp/pdfdelta-round2-holdout/jls21.pdf \
  https://docs.oracle.com/javase/specs/jls/se21/jls21.pdf
sha256sum /tmp/pdfdelta-round2-holdout/jls17.pdf /tmp/pdfdelta-round2-holdout/jls21.pdf
```

The manifest records 5,111,425 bytes and SHA-256
`11aca060e02f98da855bb86a442f8c8d330da7bde0a43584d8c413b8cd58693c` for the old PDF, and
5,221,452 bytes and SHA-256
`3157fbfb495c2c1b2c00bed1300ddfaf92376894a4fc42fb993837430b7d69b8` for the new PDF.

No comparison was run on this holdout during annotation. The existing RFC holdout remains a
separate extraction diagnostic. Its precise issue ledger is recorded in
[`prior-rfc-extraction.json`](prior-rfc-extraction.json): five unsupported issues, with the exact
operator indexes and page scopes copied from the locally captured raw result, with ledger provenance. The run produced 142
unresolved regions, zero resolved tokens on either side, incomplete extraction and comparison, zero
accepted events, and nine candidates. Its quality measurement was skipped because no expected
annotations were recorded, so these outcomes are not precision or recall measurements. The
corresponding synthetic taxonomy test is named in the ledger; it does not reproduce the RFC PDFs.
