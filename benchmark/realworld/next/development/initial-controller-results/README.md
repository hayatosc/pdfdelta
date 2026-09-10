# Initial closed-scope recovery failure

The paragraph-12 reference was fixed before either comparison. Both the pre-P3
production implementation (`3259710`, P2 experiment is test-only) and the initial
P3 implementation (`c4354d9`) were built in release mode with the committed lockfile.
The former was built from a Git archive in a separate target directory. Run records
bind the executable, PDF, annotation-reference and report hashes. The original
development baseline at `3f318d7` remains separate; these runs isolate P3 from P1.

Both shared routes detect 0/1 inspected scope changes. They report zero typed,
inferred or added scope changes, and remain incomplete with exit 3. Precision is
undefined at zero predictions. Strict event and position metrics are unknown for
this deliberately scope-only reference. Unannotated document regions are not
negative gold. Removing only elapsed time and the additive P3 fields gives exact
JSON equality of the preexisting shared-route contracts, including coverage,
candidate decisions and unresolved dependencies.

The shared routes retain 77 literal proposals and 28 accepted correspondences.
Group work reaches 999,571 token checks of the 1,000,000 limit, and the next charge
cannot fit. Optional text enumeration returns incomplete before the indexed
one-to-one text search starts: zero text pair checks and zero text token visits.
No optional proposals survive that supplier's conservative withdrawal. The P3
initial global-completeness gate therefore emits no scope review. This failure
requires follow-up on candidate completion and finite local scope closure; it is
not an adoption result or evidence of expanded source recovery.

| Implementation | Native seconds | Shared text seconds | Shared all seconds |
| --- | ---: | ---: | ---: |
| Before P3 | 31.97 | 8.39 | 8.73 |
| Initial P3 | 43.69 | 8.50 | 9.09 |

These are single observations for functional diagnosis. Builds and comparison
processes overlapped during capture; they do not support a speed comparison.
Both native reports exceed the existing 128 MiB summary input ceiling (the initial
P3 report is 791,109,475 bytes). Their output sizes, hashes, exits and process costs
remain recorded; their annotation metrics were not computed. Full raw reports
remain outside version control.

Rebuild each revision, then use a distinct output directory per executable:

```sh
PYTHON_UV=0 python benchmark/realworld/next/development/capture-comparisons.py \
  /path/to/pdfdelta benchmark/realworld/cache/next-dev /tmp/controller-runs \
  --pair edpb-controller-processor-v1-to-v2-1 --implementation <build-commit>
```

The driver checks registered input hashes and requires a fixed source reference
or explicit unresolved annotation record. It preserves all three routes, default
limits, a 180-second deadline, failed processes, output size and peak RSS. It does
not score annotations. Do not rerun these unchanged cases without a new change or
specific diagnostic need.
