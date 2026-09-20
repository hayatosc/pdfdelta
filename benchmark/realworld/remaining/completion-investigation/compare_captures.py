#!/usr/bin/env python3
"""Compare hash-bound captures without merging strict and inferred outcomes."""

import argparse
from collections import Counter
import json
from pathlib import Path

from capture import PANEL, reference, summarize


def signature(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def retained_claims(report):
    comparisons = Counter()
    reviews = Counter()
    for scope in report["comparison"]["scopes"]:
        if scope["interpretation"] != "conditional_on_correspondence":
            continue
        result = scope["result"]
        for comparison in result["comparisons"]:
            if comparison["interpretation"] == "conditional_on_correspondence":
                comparisons[signature(comparison)] += 1
        for review in result["text_scope_reviews"]:
            if review["comparison"]["interpretation"] == "conditional_on_correspondence":
                reviews[signature(review)] += 1
    return comparisons, reviews


def load_report(row):
    path = Path(row["report"]["path"])
    if reference(path) != row["report"]:
        raise ValueError(f"report hash mismatch: {path}")
    report = json.loads(path.read_text())
    summary = summarize(report)
    if summary != {key: row[key] for key in summary}:
        raise ValueError(f"summary disagrees with report: {path}")
    return report


def search_resolved(report):
    comparison = report["comparison"]
    return not comparison["relation_unresolved"] and all(
        not scope["result"]["unresolved"]
        and not scope["result"]["structural_correspondences"]
        for scope in comparison["scopes"]
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    before = json.loads(args.before.read_text())
    after = json.loads(args.after.read_text())
    for field in ("panel", "fixed_denominator", "route", "limit_scale", "timeout_seconds"):
        if before[field] != after[field]:
            raise ValueError(f"capture contract differs: {field}")
    panel = {pair["id"]: pair for pair in json.loads(PANEL.read_text())["pairs"]}
    if before["panel"] != reference(PANEL):
        raise ValueError("panel hash mismatch")
    previous = {row["pair"]: row for row in before["rows"]}
    current = {row["pair"]: row for row in after["rows"]}
    if len(previous) != len(before["rows"]) or len(current) != len(after["rows"]):
        raise ValueError("duplicate capture row")
    if not current.keys() <= previous.keys() or not current.keys() <= panel.keys():
        raise ValueError("captures contain unexpected pairs")
    result = {"version": 1, "before": reference(args.before), "after": reference(args.after),
              "script": reference(__file__), "fixed_denominator": 36,
              "full_panel": current.keys() == panel.keys(), "rows": []}
    for pair, row in current.items():
        prior = previous[pair]
        if prior["status"] != "captured" or row["status"] != "captured":
            raise ValueError(f"missing successful comparison: {pair}")
        a = load_report(prior)
        b = load_report(row)
        for report in (a, b):
            for side in ("old", "new"):
                if report[side]["revision"] != panel[pair][side]["sha256"]:
                    raise ValueError(f"report input hash mismatch: {pair}/{side}")
            for coverage in report["coverage"]:
                for side in ("old", "new"):
                    accounted = sum(coverage.get(f"{side}_{kind}_sources", 0)
                                    for kind in ("compared", "presence", "uncompared"))
                    if accounted != coverage[f"{side}_discovered_sources"]:
                        raise ValueError(f"source conservation failed: {pair}/{side}")
            complete = all(coverage["complete"] for coverage in report["coverage"]) and search_resolved(report)
            if complete != report["comparison_complete"]:
                raise ValueError(f"completion predicate disagrees: {pair}")
        a_comparisons, a_reviews = retained_claims(a)
        b_comparisons, b_reviews = retained_claims(b)
        delta = {
            "pair": pair,
            "before_complete": a["comparison_complete"],
            "after_complete": b["comparison_complete"],
            "search_resolved_before": search_resolved(a),
            "search_resolved_after": search_resolved(b),
            "lost_strict_comparisons": sum((a_comparisons - b_comparisons).values()),
            "lost_source_backed_reviews": sum((a_reviews - b_reviews).values()),
            "inferred_changes_before": a["inferred_changes"],
            "inferred_changes_after": b["inferred_changes"],
            "incomplete_components_before": len(prior["incomplete_components"]),
            "incomplete_components_after": len(row["incomplete_components"]),
            "comparison_payload_identical": a["comparison"] == b["comparison"],
            "coverage_identical": a["coverage"] == b["coverage"],
        }
        delta["source_count_delta"] = {
            side: b["coverage"][0][f"{side}_compared_sources"]
                  - a["coverage"][0][f"{side}_compared_sources"]
            for side in ("old", "new")
        }
        result["rows"].append(delta)
    result["before_complete"] = sum(row["before_complete"] for row in result["rows"])
    result["after_complete"] = sum(row["after_complete"] for row in result["rows"])
    result["regressions"] = [row["pair"] for row in result["rows"] if
        row["lost_strict_comparisons"] or row["lost_source_backed_reviews"]
        or any(delta < 0 for delta in row["source_count_delta"].values())
        or row["before_complete"] and not row["after_complete"]]
    with args.output.open("x") as stream:
        json.dump(result, stream, indent=2)
        stream.write("\n")
    print(json.dumps({key: value for key, value in result.items() if key != "rows"}, indent=2))
    return bool(result["regressions"])


if __name__ == "__main__":
    raise SystemExit(main())
