# Native projection validation before source discharge

## Correction

The new domain owner previously checked source conservation and local population
closure without directly checking the text projection against complete native
glyph readings and their order. A constructed counterexample changed a native
glyph while retaining the old graph text and still received an owned equality
domain. This was an implementation defect in the new ownership path.

The domain now invokes the existing source-side native projection validator
before comparison. It checks complete glyph readings, supported folds and source
order, while preserving unresolved normalization. The source partition continues
to account for every original reference and reject cuts through shared glyphs.
The same shared work ceiling covers these additional checks.

Contracted edge spaces require special care. Existing projection code expands
only independently verified literal-space glyphs. A domain uses the original
token addresses only when the expanded native body has the identical token
sequence; expanded outer padding remains in the complement. This neither drops
an interior literal space nor interprets a different character as padding.

## Validation and current numbers

A regression test failed before the fix: native text was changed but both domains
were owned instead of rejecting the affected domain. Current controls reject a
changed native reading, a partial glyph reading and incompatible native order,
while retaining the independent valid boundary. Further controls admit contracted
literal outer padding and reject nonspace padding and contracted interior spaces.

Final workspace tests pass: 2,496 passed, zero failed, two ignored. Clippy and
format/whitespace checks pass; generated fixtures pass 48/48, preserving the
42 strict author-intent and six candidate-policy distinction.

| Pair | Final domains | Old compared sources | New compared sources | Gain per side over original baseline |
| --- | ---: | ---: | ---: | ---: |
| EDPB controller/processor | 32 | 2,120 | 2,146 | 714 |
| EDPB restrictions | 24 | 571 | 571 | 571 |

The earlier work-accounting pilot reported gains of 748 and 893 per side.
Those are superseded: 34 and 322 references respectively return to unresolved
under the additional projection validation and the unchanged shared budget.
The declined natural domains are not claimed to be unequal or incorrect; this
path has not established them under its complete proof requirements.

An initial direct-projection attempt also rejected contracted outer padding.
Its separately archived five-pair pilot is in `domain-projection-initial-pilot.json`.
The final implementation uses the existing validated expansion described above.
The intermediate counts are not the final result.

Schedule C, Schedule SE and NIST contingency retain their previous coverage.
Every existing candidate, local comparison, B review and unresolved reason is
unchanged across the five-pair pilot. The 56 retained domains are an exact subset
of the 78 independently source-audited domains in the prior pilot. Reusing those
native-source witnesses checks their readings/conservation; the new in-process
projection validator and regression controls establish the added source-side
requirements. No fresh source-bundle acquisition or full-panel evaluation is
claimed here.

All five pairs remain incomplete. No new strict whole-document completion is
established; inventory and search obligations still prevent the goal.

## Evidence

`domain-projection-pilot.json` binds the final source archive, binary, build and
check logs, reports, previous implementation, declined domains and reused source
reviews. The pre-fix failure is retained as `regression-before.log` beside the
final cache artifacts in `benchmark/realworld/cache/source-completion/domain-projection-v4/`.
The initial attempted implementation is separately preserved in `domain-projection-v3/`.
