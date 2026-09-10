"""Join completed route summaries and strict scores without dropping failures.

Usage: python collect-final-corpus.py EVIDENCE_DIRECTORY
"""

import json
import sys
from pathlib import Path


directory = Path(sys.argv[1])


def read(name):
    return json.loads((directory / name).read_text())


baseline = read("baseline-routes.json")["routes"]
current = read("candidate5-routes.json")["routes"]
strict = read("candidate5-strict-audited-source-recovery.json")
assert {(row["pair"], row["route"]) for row in baseline} == {(row["pair"], row["route"]) for row in current}
assert sum(row["process_status"] == "finished" for row in current) == 84


def summarize(rows):
    result = {}
    for route in ("native", "shared_text", "shared_all"):
        selected = [row for row in rows if row["route"] == route]
        observations = [row["observations"] for row in selected if row["observations"] is not None]
        processes = [row["process"] for row in selected if row["process"] is not None]
        summary = {
            "inventory_entries": len(selected),
            "finished": sum(row["process_status"] == "finished" for row in selected),
            "reports": len(observations),
            "complete_reports": sum(row["comparison_complete"] is True for row in observations),
            "process_seconds_sum": round(sum(row["elapsed_seconds"] for row in processes), 2),
            "peak_rss_kib": max(row["peak_rss_kib"] for row in processes),
        }
        if route == "native":
            matches = [match for row in observations for match in (row.get("expected_matches") or [])]
            summary.update(matched_events=sum(match["matched"] is True for match in matches), expected_events=len(matches))
            for output, key in (("expected_changed_tokens", "expected_changed_tokens"), ("reported_changed_tokens", "reported_changed_tokens"), ("true_positive_changed_tokens", "true_positive_tokens")):
                summary[output] = sum((row.get("scoped_tokens") or {}).get(key, 0) for row in observations)
        else:
            for key in ("conditional_operations", "inferred_operations"):
                summary[key] = sum(row[key] for row in observations)
            for key in ("old_compared_sources", "new_compared_sources"):
                summary[key] = sum(channel[key] for row in observations for channel in row["coverage"])
        result[route] = summary
    return result


summary = {"status": "final_frozen_corpus_measurements", "baseline": summarize(baseline), "current": summarize(current), "strict_shared": [{"route": row["route"], **row["summary"]} for row in strict["routes"]]}
index = {(row["pair"], row["route"]): row for row in current}
summary["native_baseline_successes_lost"] = []
summary["shared_coverage_regressions"] = []
for before in baseline:
    after = index[before["pair"], before["route"]]
    old = before["observations"] or {}
    new = after["observations"] or {}
    if before["route"] == "native":
        retained = {match["expected_id"] for match in new.get("expected_matches", []) or [] if match["matched"]}
        summary["native_baseline_successes_lost"].extend({"pair": before["pair"], "id": match["expected_id"]} for match in old.get("expected_matches", []) or [] if match["matched"] and match["expected_id"] not in retained)
    else:
        channels = {channel["channel"]: channel for channel in new.get("coverage", [])}
        for channel in old.get("coverage", []):
            for side in ("old", "new"):
                key = f"{side}_compared_sources"
                count = channels.get(channel["channel"], {}).get(key, 0)
                if count < channel[key]:
                    summary["shared_coverage_regressions"].append({"pair": before["pair"], "route": before["route"], "channel": channel["channel"], "side": side, "baseline": channel[key], "current": count})
events = {(event["pair"], event["id"], route["route"]): event for route in strict["routes"] for event in route["events"]}
ledger = read("blocker-ledger.json")
ledger["status"] = "candidate5_final_frozen_corpus"
for expectation in ledger["expectations"]:
    for route in expectation["routes"]:
        row = index[expectation["pair"], route["route"]]
        observations = row["observations"] or {}
        route["process_status"] = row["process_status"]
        route["exit_code"] = row["process"]["exit_code"] if row["process"] else None
        route["quality_skipped_reason"] = observations.get("quality_skipped_reason")
        route["native_match"] = next((match for match in observations.get("expected_matches", []) or [] if match["expected_id"] == expectation["id"]), None)
        route["native_blockers"] = [failure for failure in (row["diagnostics"] or {}).get("failures", []) if failure["expected_id"] == expectation["id"]]
        route["shared_search"] = observations.get("scopes")
        if route["route"] == "native":
            route["mask_projection"] = {"status": "native_original_projection", "match": route["native_match"]}
        else:
            event = events[expectation["pair"], expectation["id"], route["route"]]
            route["mask_projection"] = event["current"]
            route["source_eligible"] = event["source_eligible"]
            route["source_view"] = event["source_view"]
        route["source_issues"] = {side: observations.get(f"{side}_issue_groups") for side in ("old", "new")}

for name, value in (("candidate5-comparison-summary.json", summary), ("candidate5-blocker-ledger.json", ledger)):
    (directory / name).write_text(json.dumps(value, indent=2) + "\n")
