# Static source review

The Rust CLI accepts `--review DIR` on its common evidence route. This opt-in
export reuses the completed comparison and retained evidence. It does not run
another correspondence or diff algorithm, modify masks, or change coverage and
exit codes. It requires a new directory and rejects collisions with other report
destinations before extraction. Input bytes are retained from the original read,
including stdin, so reopening a changed file cannot substitute the reviewed PDF.

The bundle contains:

- `index.html`: complete change/review entries, source locators, and dependencies.
- `old.pdf` and `new.pdf`: byte-exact compared input files.
- `comparison.json`: the existing common comparison report.
- `sources.json`: source IDs, native glyphs/raw codes, geometry, object/operator
  provenance, vectors, marked content, structured values, graph views, rendered
  metadata, profiles, warnings, and acquisition dependencies. Exported
  normalization certificates are diagnostic records, not reusable source proofs.
- `old-region-N.png` and `new-region-N.png`: lossless retained composited RGB pages.
- `manifest.json`: SHA-256 hashes and byte counts of PDF, JSON, and PNG artifacts.
  The manifest does not hash itself or the subsequently written HTML.

HTML publishes last and acts as the completion marker. The aggregate output
limit is 512 MiB; at most 1,024 page previews and 20 million source-location work
units are allowed. Writes stream through a charged writer and atomic file output.
An output failure returns an error and can leave an incomplete directory. Missing
source bounds/page locators and unavailable previews remain explicit. Native PDF
units describe aggregate source extents, not a rendered changed-ink overlay.
Form fields retain all widget locators where available.

## Display contract

Every changed local comparison, non-owning range review, changed relation, and
key-membership operation is retained; display priority never truncates them.
Unresolved comparisons and scope/acquisition dependencies remain inspectable.
Each unit has an HTML evidence ID and an exact JSON pointer into the report.
Complete source references connect back to the evidence export and original PDFs.

A uses the accepted strict operation or source-position result. B describes a
corresponding-range content difference without owning its interior edits. C keeps
an inferred counterpart. Labels and solid/double/dashed borders distinguish the
categories without relying on color. Strict local masks use solid underlines;
conditional non-owning masks use dashed underlines. Excerpts include all context
but never add context positions to a mask. Key insertion/removal labels explicitly
limit ownership to identity evidence. A/B/C are not added together as strict recall.

Text retains literal whitespace. Control, bidi-control, and private-use scalars
are displayed as escapes; raw values remain in JSON. Literal mapping/spacing
differences can leave visible words unchanged. HTML escapes all source text and
uses no JavaScript, external fonts, services, or user assertion storage. The CSP
blocks scripts and active resources. Browser-native details, page links, and
downloads work when opening the local HTML.

## Evidence and adoption

Reproduce the development sample after acquiring the frozen development inputs:

```bash
cargo build --release -p pdfdelta-cli
target/release/pdfdelta \
  benchmark/realworld/cache/next-dev/nist-sha-1803-to-1804-old.pdf \
  benchmark/realworld/cache/next-dev/nist-sha-1803-to-1804-new.pdf \
  --channels text --review /tmp/pdfdelta-nist-review-new
```

The expected incomplete exit is 3. The frozen NIST pair retains B=8, typed changes=0,
and inferred scope changes=0. Its JSON equals the previous report after removing
only `comparison_wall_time_ms`. The 75-file bundle uses 65,924,079 bytes, including
69 page PNGs. A single end-to-end sample took 4.07 seconds and 181,652 KiB peak RSS.
Executable/report/manifest hashes and checks are recorded in
`development/nist-sha-results/static-review.json`. Large local bundles and
screenshots are not committed.

CLI regression tests cover ambiguous `a` to `aa` without an owning highlight,
stdin input identity, report parity, artifact hashes, output collisions, field
changes, identity-only field membership, widget page locators, and exact RGB PNG
export for inferred image changes. Browser inspection confirmed all eight B
entries, Enter activation of a preview, local image loading, the old PDF page-7
link, and no horizontal overflow at desktop and 390px widths. Both screenshots
were visually inspected.

Adopt the opt-in export. These checks establish traceability and preserved
comparison behavior, not improved recall or measured human review time. The
remaining development/blind evaluation and visual-observer work are independent.
