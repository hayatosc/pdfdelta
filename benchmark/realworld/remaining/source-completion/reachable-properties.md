# Reachable text declarations

The bench-only paint probe now scans trailer-reachable dictionaries for
`ActualText` and `Alt`, and decodes reachable streams declared as Forms. It uses
the neutral parser facade, preserves partial observations on failure, and limits
depth, aggregate bytes, object visits and Form operators. Dictionary back-references
are deduplicated; this does not certify Form execution recursion or visibility.

The frozen six-input capture is indexed by `reachable-properties-v1.json`.
Binary, source archive, input hashes, commands and raw reports are retained under
`benchmark/realworld/cache/source-completion/reachable-properties-v1/`.

| Input | Status | Observed Form programs | Dictionary declarations | Form inline declarations |
| --- | --- | ---: | ---: | ---: |
| W-9 old | Depth limit | 21 | 0 | 0 |
| W-9 new | Depth limit | 24 | 0 | 0 |
| Schedule C old | Depth limit | 43 | 0 | 0 |
| Schedule C new | Depth limit | 43 | 0 | 0 |
| Schedule SE old | Scanned | 3 | 0 | 0 |
| Schedule SE new | Scanned | 3 | 0 | 0 |

The four depth-limited observations do not establish absence of declarations.
For SE, the bounded reachable graph contains no declarations of the inspected
kinds. Neither result establishes absence of rendered text or complete inventory.
Non-Form stream programs, invocation binding and correspondence to native sources
remain outside this diagnostic. Resource membership does not imply execution.

A generated PDF control retains a structure declaration across a catalog
back-reference and a declaration in an uninvoked Form. Both remain observations;
an unsupported Form filter leaves the scan unresolved. The three example tests,
workspace formatting, all-target Clippy and workspace tests pass. This diagnostic
changes no production comparison behavior and adds no completed natural pairs;
the latest production panel remains 0/36.
