#!/usr/bin/env python3
"""Independent endpoint-packing verification for the four mixed components.

All component candidates are weight-one literal correspondences: mostly 1:1
edges plus a few 1:N/N:1 group proposals. The verifier enumerates every
conflict-free subset of the few group proposals and solves the remaining 1:1
packing as a maximum bipartite matching. An edge is mandatory exactly when
removing it lowers the maximum matching size. This model treats endpoint
sharing as the only conflict; source conflicts and partitions are outside it.
"""

import json
import sys
from itertools import combinations

REPORT = sys.argv[1] if len(sys.argv) > 1 else (
    "benchmark/realworld/cache/completion-investigation/after/"
    "mhlw-care-skills-original-to-revised/mhlw-care-skills-original-to-revised-text.json")
TARGETS = [1306, 9649, 6161, 6177, 5831, 6130, 5619, 5690]


def endpoints(proposal):
    return set(proposal["old"]), set(proposal["new"])


def conflicting(a, b):
    ao, an = endpoints(a)
    bo, bn = endpoints(b)
    return bool(ao & bo) or bool(an & bn)


def max_matching(edges, old_ids, new_ids):
    """Kuhn's algorithm; edges are (old_id, new_id, proposal_index)."""
    adjacency = {old: [] for old in old_ids}
    for old, new, _ in edges:
        adjacency[old].append(new)
    match_new = {}
    match_old = {}

    def augment(old, seen):
        for new in adjacency[old]:
            if new in seen:
                continue
            seen.add(new)
            if new not in match_new or augment(match_new[new], seen):
                match_new[new] = old
                match_old[old] = new
                return True
        return False

    size = 0
    for old in old_ids:
        if augment(old, set()):
            size += 1
    return size, match_old


def mandatory_edges(edges, old_ids, new_ids):
    """Edges present in every maximum matching of the 1:1 graph."""
    size, _ = max_matching(edges, old_ids, new_ids)
    mandatory = set()
    for edge in edges:
        without = [other for other in edges if other != edge]
        reduced, _ = max_matching(without, old_ids, new_ids)
        if reduced < size:
            mandatory.add(edge[2])
    return size, mandatory


def verify_component(proposals, indices, labels):
    assert all(proposals[i]["weight"] == 1 and proposals[i]["basis"] == "literal_content"
               for i in indices), "oracle requires unit-weight literal candidates"
    groups = [i for i in indices if (len(proposals[i]["old"]), len(proposals[i]["new"])) != (1, 1)]
    assert len(groups) <= 12 and len(indices) <= 1000, "oracle work limit"
    singles = [i for i in indices if i not in groups]
    old_ids = sorted({n for i in indices for n in proposals[i]["old"]})
    new_ids = sorted({n for i in indices for n in proposals[i]["new"]})
    best = -1
    tied = []
    for count in range(len(groups) + 1):
        for chosen in combinations(groups, count):
            if any(conflicting(proposals[a], proposals[b]) for a, b in combinations(chosen, 2)):
                continue
            chosen_old = set().union(*[set(proposals[i]["old"]) for i in chosen]) if chosen else set()
            chosen_new = set().union(*[set(proposals[i]["new"]) for i in chosen]) if chosen else set()
            edges = []
            for i in singles:
                old, new = proposals[i]["old"][0], proposals[i]["new"][0]
                if old in chosen_old or new in chosen_new:
                    continue
                if any(conflicting(proposals[i], proposals[j]) for j in chosen):
                    continue
                edges.append((old, new, i))
            size, mandatory = mandatory_edges(edges, old_ids, new_ids)
            total = len(chosen) + size
            if total > best:
                best = total
                tied = [(chosen, mandatory)]
            elif total == best:
                tied.append((chosen, mandatory))
    common = None
    for chosen, mandatory in tied:
        selected = set(chosen) | set(mandatory)
        common = selected if common is None else common & selected
    return {
        "size": len(indices),
        "groups": groups,
        "optimum": best,
        "optimal_branches": len(tied),
        "mandatory": sorted(common),
    }


def main():
    report = json.load(open(REPORT))
    scope = report["comparison"]["scopes"][0]
    result = scope["result"]
    proposals = result["candidates"]["proposals"]
    found = {}
    for component in result["matching"]["components"]:
        indices = component["proposals"]
        if not (set(indices) & set(TARGETS)):
            continue
        outcome = verify_component(proposals, indices, component)
        outcome["production_mandatory"] = component["mandatory"]
        outcome["production_exhaustive"] = component["exhaustive"]
        outcome["production_algorithm"] = component["algorithm"]
        outcome["agrees"] = (
            component["exhaustive"]
            and sorted(component["mandatory"]) == outcome["mandatory"])
        assert outcome["agrees"], "mandatory set disagrees with endpoint oracle"
        for target in sorted(set(indices) & set(TARGETS)):
            found[target] = component["mandatory"]
        print(json.dumps(outcome))
    print("targets checked:", sorted(found))
    assert sorted(found) == sorted(TARGETS), "target set mismatch"
    for target in TARGETS:
        assert target in found[target], f"target {target} not in production mandatory"


if __name__ == "__main__":
    main()
