#!/usr/bin/env python3
"""Check small endpoint packings independently by enumerating combinations.

The two frozen natural components have at most two simultaneously selected
proposals. This verifier enumerates every subset up to that independently
derived cardinality bound; it does not reproduce a clique-choice traversal.
The native constraint projection must have no partitions, descendant ownership,
shared node sources, or source conflicts. It is not a visual transcription or
whole-document acquisition certificate.
"""

import argparse
from itertools import combinations, product
import json
from math import comb
from pathlib import Path

from capture import reference


CASES = [
    ("ecb-annual-2023-to-2024", 1437, "ecb-constraints.json"),
    ("nist-contingency-34-to-r1", 228, "contingency-constraints-v2.json"),
]


def packing(proposals):
    assert proposals and all(p["basis"] == "literal_content" and p["weight"] == 1
                             for p in proposals)
    endpoints = {side: [set(p[side]) for p in proposals] for side in ("old", "new")}
    for side in endpoints:
        assert all(endpoints[side])
        assert all(len(ids) == len(p[side]) for ids, p in zip(endpoints[side], proposals))
    bound = min(len(set.union(*sets)) // min(map(len, sets))
                for sets in endpoints.values())
    subsets = sum(comb(len(proposals), size) for size in range(bound + 1))
    assert subsets <= 100_000, "independent enumeration work limit"
    best = -1
    mandatory = set()
    optima = 0
    feasible = 0
    for size in range(bound + 1):
        for chosen in combinations(range(len(proposals)), size):
            if any(endpoints[side][a] & endpoints[side][b]
                   for side in endpoints for a, b in combinations(chosen, 2)):
                continue
            feasible += 1
            if size > best:
                best, mandatory, optima = size, set(chosen), 1
            elif size == best:
                mandatory.intersection_update(chosen)
                optima += 1
    return {"cardinality_upper_bound": bound, "subsets_examined": subsets,
            "feasible_subsets": feasible, "optimum": best,
            "optimal_subsets": optima, "mandatory_local_ids": sorted(mandatory)}


def constraints(probe, proposals, report):
    for side in ("old", "new"):
        assert probe[side + "_revision"] == report[side]["revision"]
        value = probe[side]
        ids = {node for p in proposals for node in p[side]}
        nodes = {node["id"]: node for node in value["nodes"]}
        assert set(nodes) == ids
        assert not value["shared_sources"]
        assert not value["alternatives"]
        assert not value["source_conflicts"]
        assert all(edge["from"] not in ids for edge in value["contains_edges"])
        sources = set()
        for node in nodes.values():
            assert node["basis"]["kind"] == "native_layout"
            assert node["content"]["kind"] == "text"
            assert node["content"]["view"]["normalization"]["kind"] == "exact"
            for source in node["sources"]:
                assert source["origin"] == "native"
                assert source["glyph"] not in sources
                sources.add(source["glyph"])
    assert probe["native_glyph_counts"] == [report[s]["native_glyphs"] for s in ("old", "new")]


def self_test():
    groups = [[0], [1], [0, 1]]
    universe = [{"old": old, "new": new, "basis": "literal_content", "weight": 1}
                for old, new in product(groups, repeat=2)]
    cases = 0
    for size in range(1, len(universe) + 1):
        for chosen in combinations(universe, size):
            best, common, optima = -1, set(), 0
            # This control enumerates all 2**n subsets without a cardinality bound.
            for bits in range(1 << len(chosen)):
                selected = [i for i in range(len(chosen)) if bits & (1 << i)]
                owned = {"old": [], "new": []}
                for i in selected:
                    for side in owned:
                        owned[side].extend(chosen[i][side])
                if any(len(ids) != len(set(ids)) for ids in owned.values()):
                    continue
                if len(selected) > best:
                    best, common, optima = len(selected), set(selected), 1
                elif len(selected) == best:
                    common.intersection_update(selected)
                    optima += 1
            actual = packing(chosen)
            assert (actual["optimum"], actual["mandatory_local_ids"], actual["optimal_subsets"]) == (
                best, sorted(common), optima)
            cases += 1
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("probes", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--after", type=Path)
    args = parser.parse_args()
    records = []
    for pair, component_index, probe_name in CASES:
        before_path = args.baseline / pair / (pair + "-text.json")
        report = json.loads(before_path.read_text())
        result = report["comparison"]["scopes"][0]["result"]
        component = result["matching"]["components"][component_index]
        ids = component["proposals"]
        proposals = [result["candidates"]["proposals"][index] for index in ids]
        assert not (set(ids) & set(result["matching"]["inferred_proposals"]))
        probe_path = args.probes / probe_name
        probe = json.loads(probe_path.read_text())
        constraints(probe, proposals, report)
        outcome = packing(proposals)
        mandatory = [ids[index] for index in outcome["mandatory_local_ids"]]
        record = {"pair": pair, "before": reference(before_path),
                  "probe": reference(probe_path), "component": component_index,
                  "proposal_count": len(proposals), **outcome,
                  "mandatory_proposal_ids": mandatory}
        if args.after:
            after_path = args.after / pair / (pair + "-text.json")
            after = json.loads(after_path.read_text())
            newer = after["comparison"]["scopes"][0]["result"]
            assert newer["candidates"] == result["candidates"]
            matches = [c for c in newer["matching"]["components"] if c["proposals"] == ids]
            assert len(matches) == 1
            actual = matches[0]
            assert actual["exhaustive"] and actual["mandatory"] == mandatory
            record.update(after=reference(after_path), production_agrees=True,
                          production_algorithm=actual["algorithm"],
                          production_states=actual["explored_states"],
                          production_assignment_work=actual["assignment_work"])
        records.append(record)
    result = {"version": 1, "script": reference(__file__), "scope": __doc__,
              "exhaustive_control_cases": self_test(),
              "cases": records,
              "limitations": "The constraint projection uses the production native parser and "
              "layout; it does not independently adjudicate omitted rivals, semantic identity, "
              "full-graph structured views, visual readings, or complete inventory."}
    with args.output.open("x") as stream:
        json.dump(result, stream, indent=2)
        stream.write("\n")
    print(json.dumps(records, indent=2))


if __name__ == "__main__":
    main()
