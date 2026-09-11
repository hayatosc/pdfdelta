# Development input registration

`selection.json` records 24 initial publication-series slots and two replacement
attempts selected before comparison output. `inputs.json` freezes 24 active pairs
and the SHA-256 hashes of all 48 available PDFs. This freezes development inputs,
not implementation or blind evaluation. No comparison of this set has run at
registration time; source annotations and concrete presentation-trait inspection
remain pending. Family labels record intended coverage, not verified capabilities.

CLIP (2103.00020) and LLaMA (2302.13971) have only v1 in their arXiv submission
histories; requested v2 downloads returned HTTP 404. Their original records remain,
with DDPM (2006.11239 v1/v2) and Llama 2 (2307.09288 v1/v2) as replacements.
The corresponding arXiv abstract pages provide the revision histories.

`acquisition-attempts.json` retains failed URLs and successful retries, including
HTTP 429 and the obsolete NIST PDF location. Publisher-hosted bytes are cached at
`benchmark/realworld/cache/next-dev/<pair-id>-<old|new>.pdf` and are not committed.
Reproduction must verify the recorded hash, not assume a stable URL serves stable
bytes. The care-skills original-series PDF is the October 2020 update, while the
new file is the March 2025 revision. Curriculum years and NASA revision years name
the publication series; downloaded corrected editions must be distinguished during
source annotation rather than treated as the first bytes published that year.

Preserve unsupported extraction and unresolved annotations in later denominators.
These records are development data and must never be relabeled as blind.

## Source inspection and first reference

`glyphs-inspection.json` records 48 attempted source inspections: 47 captured
outputs and one failed extraction. The ECB 2023 input exceeds the existing 65,536
CID-width-entry limit; its counts are null, not zero evidence. Among captured
outputs, 16 report extraction issues and 11 contain unmapped glyphs (overlapping
populations). Only 27/48 inputs have neither reported condition. This is an
inspection observation, not proof of complete visible content. All 48 object
inspections captured page metadata. Process maximum RSS is not live heap telemetry.

The first fixed reference is the controller/processor guideline's paragraph 12,
with adjacent unchanged paragraphs 11 and 13. Its seven literal selectors resolve
uniquely, with uncompressed text, scalar/UTF-8 offsets and complete glyph atom
lists retained in `annotations/`. Compact `source_rows` retain each position's
full atom list; scalar values are recovered from the literal quote. The expected
content addition was inspected in both source text and rendered page 9 before any
comparison. The visible old text lacks many real space glyphs; the new revision
contains them. The annotation therefore does not equate paint-order strings with
rendered prose or certify an exact minimal-edit mask. It fixes one scope-content
target, not exhaustive whole-document gold or a strict-position denominator.

Reproduce raw source captures in new directories (GNU time and timeout required):

```sh
PYTHON_UV=0 python benchmark/realworld/next/development/inspect-sources.py \
  target/release/pdfdelta benchmark/realworld/cache/next-dev /tmp/dev-glyphs
PYTHON_UV=0 python benchmark/realworld/next/development/inspect-sources.py \
  target/release/pdfdelta benchmark/realworld/cache/next-dev /tmp/dev-objects --view objects
target/debug/pdfbench validate-literal-selectors \
  --annotation benchmark/realworld/next/development/annotations/edpb-controller-processor-v1-to-v2-1.json \
  --old benchmark/realworld/cache/next-dev/edpb-controller-processor-v1-to-v2-1-old.pdf \
  --new benchmark/realworld/cache/next-dev/edpb-controller-processor-v1-to-v2-1-new.pdf
```

The inspection runner enforces the existing parser/extractor limits, a 180-second
process deadline and a 256 MiB raw-output ceiling. Failures and truncated captures
remain attempted observations. Raw glyph logs total about 2 GiB and stay outside
version control. The records bind their bytes and the actual executable hash.
Initial source captures used the pre-P3 executable; extraction was unchanged by
P3. Render checks used the existing worker's default white-background RGB profile;
warning flags, dimensions, page objects and pixel-response hashes are in the
reference record. This small visual check does not establish complete rendering
feature coverage or satisfy the separate observer experiment.

`initial-controller-results/` records the first comparison after this reference
was fixed; `native-closure-results/` records the later local-closure implementation
on that same reference. Both still miss its frozen paragraph target.

The second frozen reference is the NIST SHA abstract, with seven unique literal
selectors and both source pages visually checked before comparison. Its frozen
target is also missed. `nist-sha-results/` separately adjudicates all eight new
scope reviews after comparison: six visibly changed body ranges, one whitespace
case and one glyph-mapping case with unchanged visible inequalities. Those posthoc
judgments do not enlarge the preregistered recall denominator or imply blind
validation. The two shared routes reuse the same eight predictions.

The other 22 pairs still need source references and have not been compared.
Concrete vertical/ruby/scan/overpaint traits and the remaining evaluation
denominators are not established by family labels.
