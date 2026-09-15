"""Read independent completion obligations without changing comparison claims.

Common reports do not contain the full evidence graph. Scope observations below
locate potential dependencies; they do not assign every residual source a cause.
"""

import argparse
from collections import Counter
import json
from pathlib import Path


def audit_report(report):
    """Require a common report and preserve simultaneous blocking conditions."""
    coverage = report["coverage"]
    channels = report["contract"]["channels"]
    if not coverage or sorted(row["channel"] for row in coverage) != sorted(channels):
        raise ValueError("coverage must contain each selected channel exactly once")
    if len(set(channels)) != len(channels):
        raise ValueError("selected channels must be unique")
    inventories = True
    sources = True
    for row in coverage:
        for side in ("old", "new"):
            inventories &= row[f"{side}_inventory_complete"]
            counts = [row[f"{side}_{name}_sources"] for name in
                      ("discovered", "compared", "uncompared")]
            presence = row.get(f"{side}_presence_sources", 0)
            if any(type(n) is not int or n < 0 for n in counts + [presence]):
                raise ValueError("source counts must be nonnegative integers")
            if counts[0] != counts[1] + counts[2] + presence:
                raise ValueError("source accounting does not conserve discovered references")
            sources &= counts[2] == 0
        expected = (row["old_inventory_complete"] and row["new_inventory_complete"]
                    and row["old_uncompared_sources"] == row["new_uncompared_sources"] == 0)
        if row["complete"] != expected:
            raise ValueError("channel completion disagrees with its obligations")
    comparison = report["comparison"]
    search = not comparison["relation_unresolved"]
    scopes = []
    for index, scope in enumerate(comparison["scopes"]):
        result = scope["result"]
        search &= not result["unresolved"] and not result["structural_correspondences"]
        components = result["matching"]["components"]
        scopes.append({
            "pointer": f"/comparison/scopes/{index}/result",
            "interpretation": scope["interpretation"],
            "unresolved_reasons": dict(Counter(result["unresolved"])),
            "structural_correspondences": result["structural_correspondences"],
            "components": len(components),
            "incomplete_components": sum(not c["exhaustive"] for c in components),
            "exhaustive_without_mandatory": sum(
                c["exhaustive"] and not c["mandatory"] for c in components),
            "non_owning_reviews": len(result.get("text_scope_reviews", [])),
            "text_boundary_correspondences": result.get("text_boundary_correspondences", []),
            "extraction_dependencies": result.get("extraction_dependencies", []),
            "source_cut_search": result.get("source_cut_search"),
        })
    complete = inventories and sources and search
    if report["comparison_complete"] != complete:
        raise ValueError("document completion disagrees with coverage and search")
    return {
        "comparison_complete": complete,
        "obligations": {"inventory": inventories, "source": sources, "search": search},
        "coverage": coverage,
        "discovery_observations": {
            side: {
                "inventories": report.get(side, {}).get("inventories"),
                "non_text_paint_pages": report.get(side, {}).get("non_text_paint_pages"),
                "issues": report.get(side, {}).get("issues"),
            }
            for side in ("old", "new")
        },
        "relation_unresolved": comparison["relation_unresolved"],
        "scopes": scopes,
        "source_cause_assignment": "not_evaluated_without_evidence_graph",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reports", nargs="+", type=Path)
    args = parser.parse_args()
    rows = []
    for path in args.reports:
        try:
            row = {"path": str(path), "audit": audit_report(json.loads(path.read_text()))}
        except (OSError, ValueError, KeyError, TypeError) as error:
            row = {"path": str(path), "status": "unverified", "reason": str(error)}
        rows.append(row)
    print(json.dumps({"reports": rows}, indent=2))
    return int(any("audit" not in row for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
