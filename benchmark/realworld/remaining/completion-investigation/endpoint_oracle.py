#!/usr/bin/env python3
"""Independently solve a bounded, similarity-only endpoint-packing component.

This checks the objective and mandatory proposal intersection, conditional on
independent endpoint ownership. It cannot validate PDF source independence,
partitions, reading order, or the semantic correctness of a correspondence.
"""

import argparse
from functools import lru_cache
import json
from pathlib import Path

from capture import reference


def solve(proposals, indices):
    if any(proposals[index]["basis"] != "text_similarity" for index in indices):
        raise ValueError("the oracle requires one similarity objective class")
    old = sorted({node for index in indices for node in proposals[index]["old"]})
    new = sorted({node for index in indices for node in proposals[index]["new"]})
    if len(old) > 16 or len(new) > 16:
        raise ValueError("oracle endpoint limit exceeded")
    old_bits = {node: 1 << index for index, node in enumerate(old)}
    new_bits = {node: 1 << index for index, node in enumerate(new)}
    by_old = {bit: [] for bit in old_bits.values()}
    for index in indices:
        proposal = proposals[index]
        old_mask = sum(old_bits[node] for node in proposal["old"])
        new_mask = sum(new_bits[node] for node in proposal["new"])
        for node in proposal["old"]:
            by_old[old_bits[node]].append((index, old_mask, new_mask, proposal["weight"]))
    states = 0

    @lru_cache(maxsize=None)
    def visit(remaining_old, used_new):
        nonlocal states
        states += 1
        if states > 1_000_000:
            raise ValueError("oracle state limit exceeded")
        if not remaining_old:
            return 0, frozenset()
        first = remaining_old & -remaining_old
        best, mandatory = visit(remaining_old ^ first, used_new)
        for index, old_mask, new_mask, weight in by_old[first]:
            if remaining_old & old_mask != old_mask or used_new & new_mask:
                continue
            score, selected = visit(remaining_old ^ old_mask, used_new | new_mask)
            score += weight
            selected = selected | {index}
            if score > best:
                best, mandatory = score, selected
            elif score == best:
                mandatory = mandatory & selected
        return best, mandatory

    score, mandatory = visit((1 << len(old)) - 1, 0)
    return {"score": score, "mandatory": sorted(mandatory), "states": states,
            "old_endpoints": len(old), "new_endpoints": len(new)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    report = json.loads(args.report.read_text())
    records = []
    for scope_index, scope in enumerate(report["comparison"]["scopes"]):
        result = scope["result"]
        proposals = result["candidates"]["proposals"]
        for component_index, component in enumerate(result["matching"]["components"]):
            indices = component["proposals"]
            if not indices or any(proposals[index]["basis"] != "text_similarity" for index in indices):
                continue
            oracle = solve(proposals, indices)
            oracle.update(scope=scope_index, component=component_index,
                          production_mandatory=component["mandatory"],
                          production_exhaustive=component["exhaustive"],
                          production_agrees=component["exhaustive"] and component["mandatory"] == oracle["mandatory"])
            records.append(oracle)
    output = {"version": 1, "report": reference(args.report), "script": reference(__file__),
              "contract": "similarity-only-independent-endpoint-packing-v1", "components": records}
    with args.output.open("x") as stream:
        json.dump(output, stream, indent=2)
        stream.write("\n")
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
