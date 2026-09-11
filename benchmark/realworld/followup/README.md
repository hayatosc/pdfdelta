# Fixed-source follow-up evaluation

This run has not met its recovery or completion gates. The root `PLAN.md` records
the approved thresholds and append-only progress. Historical evidence under
`../next` remains unchanged; its exposed blind inputs are development inputs now.

- `baseline.json` fixes the executable, input policy, resource configuration and
  12-hour execution deadline.
- `panel.json` retains all 36 pairs for document-wide common-text completion.
- `targets.json` binds historical literal annotations, scalar/source resolution,
  finite required cores, permissible extents and unchanged controls by hash.
  It retains unresolved targets. The 28 prose candidates include one unresolved
  source target; cover dates, numeric form targets and citation-only changes do
  not count as prose recovery. Publisher aliases share an independent-producer ID.
- `controls.json` binds the 60 generated controls and three real-source
  metamorphic controls. These are separate from natural-pair recovery counts.
- `baseline-observations.json`, when present, binds two common-text attempts per
  panel pair. An explicit process failure remains an incomplete observation.

For B scoring, the complete finite paragraph is a content-change witness, not an
assertion that every character changed. Both source cores must be covered and
the reported range must stay within the declared extents. Intervening unselected
sources cannot be added implicitly. Exact A scoring requires independent exact
gold; range annotations do not manufacture minimal edit histories or strict masks.

Run the checker with `PYTHON_UV=0 python benchmark/realworld/followup/verify.py
--stage registration`. Later stages deliberately fail until their source-bound
diagnosis, recovery and correctness evidence is implemented and registered.
`test_verify.py` checks missing/stale evidence, duplicate counting, scalar
multiplicity and nonempty complete coverage. Its synthetic examples never count
as successful PDF comparisons.

PDFs, binaries and full reports remain under ignored `../cache/followup-*` and
the existing historical cache. No external publication is part of this run.
