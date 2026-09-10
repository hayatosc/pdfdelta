"""Check necessary native scalar coverage; never certify scope completeness."""

import argparse
import hashlib
import json
from pathlib import Path


def covered_sources(report, source_map, sources):
    nodes = source_map["nodes"]
    covered = set()
    for scope in report["comparison"]["scopes"]:
        if scope["interpretation"] != "conditional_on_correspondence":
            continue
        for comparison in scope["result"]["comparisons"]:
            mask = comparison.get("text_mask")
            if (not comparison["compared"] or comparison["unresolved"]
                    or comparison["interpretation"] != "conditional_on_correspondence"
                    or mask is None or mask["claims"]["changed_source_upper"] != 0):
                continue
            if any(not comparison[side] or any(str(n) not in nodes for n in comparison[side])
                   for side in ("old", "new")):
                continue
            if any(sum(nodes[str(n)]["token_count"] for n in comparison[side])
                   != len(mask["claims"]["mandatory_" + side]) for side in ("old", "new")):
                continue
            for index, source in enumerate(sources):
                atoms = source["atoms"]
                if not atoms or any(a["kind"] != "glyph" for a in atoms):
                    continue
                # Controls use the same source map on both sides. Require each
                # reviewed scalar to be witnessed on both sides of one comparison.
                if all(any(all(
                    len(binding := nodes[str(n)]["single_scalar_bindings"].get(str(a["id"]), [])) == 1
                    and binding[0]["value"] == source["value"]
                    and 0 <= binding[0]["token"] < nodes[str(n)]["token_count"]
                    for a in atoms) for n in comparison[side]) for side in ("old", "new")):
                    covered.add(index)
    return covered


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("source_map", type=Path)
    parser.add_argument("selectors", type=Path)
    parser.add_argument("side", choices=("old", "new"))
    args = parser.parse_args()
    paths = [args.report, args.source_map, args.selectors]
    report, source_map, selectors = [json.loads(p.read_bytes()) for p in paths]
    if report["old"]["revision"] != report["new"]["revision"]:
        parser.error("This diagnostic accepts identical-input controls only")
    if report["old"]["revision"] != selectors[args.side]["sha256"]:
        parser.error("Selector input does not match the control input")
    sources = [s for e in selectors["selectors"]["expectations"] for s in e[args.side]["sources"]]
    covered = covered_sources(report, source_map, sources)
    print(json.dumps({
        "status": "necessary_scalar_coverage_only_not_acceptance",
        "inputs": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths},
        "reviewed_scalars": len(sources), "covered_scalars": len(covered),
        "missing_scalar_indices": sorted(set(range(len(sources))) - covered),
        "complete_scope_pass": False,
        "limitations": ["Source map must be reconstructed with the exact report library and configuration.",
                        "This does not certify extraction inventory or correspondence identity."]
    }, indent=2))


if __name__ == "__main__":
    main()
