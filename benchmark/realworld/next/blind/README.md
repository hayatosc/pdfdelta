# Frozen blind input registration

Implementation and executable hashes in `freeze.json` were recorded before
selecting these publication series or inspecting comparison output. The active
inputs contain **12 pairs, two per intended family**. `selection.json` preserves
the original selection; `replacement-selection.json` preserves six replaced
attempts and their reasons, including the unsuccessful SIP replacement.
`inputs.json` binds every active old/new PDF to its URL, SHA-256 and acquisition
ledger. Filter out entries containing `replacement` to obtain the active set.

`annotation-freeze.json` binds all source annotations, resolutions and expected
outcomes before the first blind comparison. The frozen baseline and current
executables then run sequentially with identical routes and limits. Family labels
describe intended sampling, not demonstrated extraction or comparison support.

## Frozen annotation denominators

There is one finite changed-content target per pair, plus unchanged controls in
six pairs. Literal resolution succeeds for 11/12 pairs and 34/36 selectors. The
two kana selectors remain missing; visually transcribed quotes are retained with
no fabricated glyph IDs. Its target remains an attempted scope with unavailable
native-source localization. Both sides have visible content despite zero glyphs.

Only the selected W-2 Copy A year has strict internal-position gold: one unique
replacement event, two side-qualified glyph atoms and two Unicode scalars.
The remaining 32 glyphs per side in that label are unchanged. Other year copies,
OMB-number changes and unannotated predictions are outside this restricted gold.
The other eleven content targets do not assert unique internal edit positions.

`score.py` measures source-set containment and excess context against those
frozen scopes. Partial overlap is not a successful full-target recovery; a new
review outside the annotation remains unscored until separately adjudicated.
Legacy inferred text's literal-quote containment is only a text diagnostic, not
proof of counterpart identity or source localization. No whitespace normalization
is introduced by this scorer. Native proven regions remain separate from B.

The original GNU PDF paths and several historical RFC PDF routes returned 404.
The ResNet publication history has no second revision. Dated complete postal
terms could not be located in the primary-site search. These records remain in
the acquisition ledgers. NIST controls and contingency-planning publications,
Faster R-CNN, and archived Japanese kana guidance fill the corresponding slots.
NIST risk-assessment guidance replaces the unavailable HTTP revision pair.

## Source inspection

All 24 active PDFs were acquired and hash-checked. Object inspection completed
for 24/24 inputs. Glyph-output capture completed for 22/24; both SP 800-53
captures reached the explicit 256 MiB output ceiling and exited 153. This is an
inspection-output failure, not evidence of empty or absent source text. The
early captured introduction pages remain available for annotation preparation.

The kana pair has zero acquired glyphs on both sides. The old source visibly
contains vertical scanned text on rotated pages; the new source visibly
contains horizontal text and ruby, with eleven extraction issues. FAA's old
Thunderstorms PDF contains a scanned page and a text layer whose words differ
from what is visible. Extraction success must not be interpreted as visible
transcription accuracy. `presentation-observations.json` records only pages
actually inspected visually; successful raster creation alone is not a visual
inspection.

The 1952 writing-guidance input is the publisher's annotated reprint with later
reading substitutions, not an untouched original. Form 1099-MISC inputs are
the January 2024 and April 2025 revisions. Exact bytes, rather than shorthand
pair names, define all inputs.

Initial page renders used the existing object-inspection output for dimensions.
Two kana-new page requests failed because their CropBox is indirect. A separate
native metadata acquisition resolved the dimensions; both corrected requests
then rendered successfully. The failed requests are retained. The worker checks
the input page count, object identity and dimensions before returning RGB.

## Reproduction

`download-selection-initial.json`, numbered download selections, and the final
`download-selection.json` record acquisition batches. Run `download.py` with a
selected batch restored as `download-selection.json` in a fresh checkout/cache;
it refuses to overwrite either its ledger or an existing destination. Archive
each completed ledger before the next batch. Null sides mean no URL was
requested in that batch; consult earlier ledgers for already acquired sides.
The sandbox DNS failures are also retained separately.

```sh
PYTHON_UV=0 python benchmark/realworld/next/development/inspect-sources.py \
  target/release/pdfdelta benchmark/realworld/cache/next-blind \
  /tmp/blind-glyphs --manifest benchmark/realworld/next/blind/inputs.json
PYTHON_UV=0 python benchmark/realworld/next/development/inspect-sources.py \
  target/release/pdfdelta benchmark/realworld/cache/next-blind \
  /tmp/blind-objects --manifest benchmark/realworld/next/blind/inputs.json --view objects
PYTHON_UV=0 python benchmark/realworld/next/blind/render-pages.py \
  benchmark/realworld/next/blind/render-selection.json /tmp/blind-renders
```

The rendering helper reads the registered PDFs and object captures from
`benchmark/realworld/cache/next-blind*`. The additional body-page selection is
`render-selection-body.json`; its kana records also bind the native metadata
response. Obtain that response by sending a newline-terminated JSON request
`{"job":{"kind":"metadata","forms":false},"password":null,"font_identities":[],"cache_dir":null}`
followed by the original PDF bytes to the frozen executable's `acquire-native`
worker. This acquires page metadata without alignment or comparison.

All renders use the frozen executable's 72 dpi, white-background profile and
default annotation/display state. Warning flags, dimensions and response hashes
remain in `render-attempts.json` and `body-render-attempts.json`. PDF bytes and
raster outputs remain in the ignored cache; the committed ledgers are the
reproducibility record. No accuracy, complete-inventory or speed claim is made
from these source-only observations.

After rebuilding the two recorded revisions, substitute their executable paths
in this replay snippet. The retained hashes identify the measured binaries;
rebuild hashes may differ with the host/toolchain and must be recorded separately.
Use fresh output directories and keep the executions sequential.

```sh
PYTHON_UV=0 python - <<'PY'
import json
from pathlib import Path
import subprocess

root = Path('benchmark/realworld/next/blind')
pairs = json.loads((root / 'annotation-freeze.json').read_text())['pairs']
selection = [value for pair in pairs for value in ('--pair', pair['id'])]
for revision, binary, commit in (
    ('baseline', '/path/to/baseline/pdfdelta', '3f318d7fd5cb0911e78f47fe05ea4460ae54c633'),
    ('current', '/path/to/current/pdfdelta', 'b258a7b69ea0c2a81796330265572dfb2043476a'),
):
    subprocess.run([
        'python', 'benchmark/realworld/next/development/capture-comparisons.py',
        binary, 'benchmark/realworld/cache/next-blind', '/tmp/blind-' + revision,
        '--manifest', str(root / 'inputs.json'), '--implementation', commit,
        *selection,
    ], check=True)
PY
PYTHON_UV=0 python benchmark/realworld/next/blind/score.py \
  /tmp/blind-baseline /tmp/blind-current /tmp/blind-scores
```

The scorer verifies the original executable hashes as well as input, annotation
and report hashes. A different rebuilt executable is a new experiment and must
not overwrite the frozen record. Raw native reports can exceed 10 GB; the scorer
memory-maps only bounded summary/change/extraction fields using the recorded
serializer format. Shared reports use ordinary JSON decoding. Whole-report
hash verification still reads every byte. No PDF comparison is run by scoring.
