"""Retain the frozen source eligibility and baseline while collecting a replay.

Usage: python curate-source-evaluation.py FROZEN_DIRECTORY OUTPUT_DIRECTORY
Every scheduled source validation must have a terminal outcome before collection.
"""

import copy
import json
import sys
from pathlib import Path


root, output = map(Path, sys.argv[1:])
evidence = Path(__file__).parent
frozen = json.loads((evidence / "candidate4-strict-audited-source-recovery.json").read_text())
collected = []
evaluations = {}
for task in (root / "score-tasks.txt").read_text().splitlines():
    annotation, route, view = task.split()
    pair = json.loads(Path(annotation).read_text())["pair"]
    trial = root / f"source-evaluation-{view}" / pair / route
    code = (trial / "exit-code.txt").read_text().strip()
    row = {"pair": pair, "route": route, "source_view": view, "exit_code": code}
    row["stderr"] = (trial / "stderr.log").read_text()
    process = trial / "process.json"
    row["process"] = json.loads(process.read_text().splitlines()[-1]) if process.exists() else None
    if code == "0":
        report = json.loads((trial / "report.json").read_text())
        row.update(old_source=report["old"], new_source=report["new"], evaluation=report["evaluation"])
        evaluations[pair, route, view] = report["evaluation"]
    collected.append(row)

result = copy.deepcopy(frozen)
result["status"] = "frozen_candidate5_failure_inclusive_development_comparison"
result["eligibility_source"] = "candidate4-strict-audited-source-recovery.json"
result["historical_metric"] = frozen["historical_metric"]
for route in result["routes"]:
    for kind, evaluation_key in (("events", "expectations"), ("scopes", "scopes")):
        for item in route[kind]:
            view = "paint-order" if item["source_view"] == "native_page_paint_order_scalars_v1" else "layout"
            evaluation = evaluations.get((item["pair"], route["route"], view))
            current = None
            if evaluation is not None:
                current = next(entry for entry in evaluation[evaluation_key] if entry["id"] == item["id"])
            if kind == "events":
                item["current"] = {
                    "evaluation": current,
                    "recovered": item["source_eligible"] and current is not None and current["matched"] is True,
                }
            else:
                item["current"] = current
    summary = route["summary"]
    summary["current_recovered_events"] = sum(item["current"]["recovered"] for item in route["events"])
    tokens = [item["current"]["tokens"] for item in route["scopes"] if item["current"] and item["current"]["tokens"]]
    summary["tokens"]["current_reported"] = sum(item["reported_changed_tokens"] for item in tokens)
    summary["tokens"]["current_true_positive"] = sum(item["true_positive_tokens"] for item in tokens)
    summary["tokens"]["current_false_positive"] = sum(item["reported_changed_tokens"] - item["true_positive_tokens"] for item in tokens)
    summary["current_available_scopes"] = len(tokens)
    summary["declared_scopes"] = len(route["scopes"])
    summary["baseline_successes_lost"] = [item["id"] for item in route["events"] if item["baseline"]["recovered"] and not item["current"]["recovered"]]

output.mkdir(parents=True, exist_ok=True)
for name, value in (("candidate5-strict-source-evaluation.json", {"schema_version": 1, "routes": collected}), ("candidate5-strict-audited-source-recovery.json", result)):
    (output / name).write_text(json.dumps(value, indent=2) + "\n")
