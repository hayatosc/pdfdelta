"""Compare captured coverage and reviewed quality without treating null as zero."""

import argparse
import csv
import hashlib
import json
from pathlib import Path


root = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--before", default="before", help="Baseline directory relative to this script")
parser.add_argument("--after", default="after", help="Capture directory relative to this script")
parser.add_argument("--output", default="comparison.json", help="Result path relative to this script")
args = parser.parse_args()
manifest = root.parents[1] / "manifest.tsv"
with manifest.open() as source:
    pairs = {
        row["pair_id"]
        for row in csv.DictReader(
            (line for line in source if line.strip() and not line.startswith("#")),
            delimiter="\t",
        )
    }

paths = {
    "before": root / args.before / "all.json",
    "after": root / args.after / "all.json",
}
captures = {side: json.loads(path.read_text()) for side, path in paths.items()}
for side, capture in captures.items():
    if set(capture) != pairs:
        raise SystemExit(f"Incomplete {side} capture: {set(capture) ^ pairs}")


def record(capture, pair):
    records = capture[pair]["records"]
    if len(records) != 1 or records[0]["pair_id"] != pair:
        raise ValueError(f"Unexpected record identity: {pair}")
    return records[0]


def metric(record, section, field):
    values = record.get(section)
    return values.get(field) if values is not None else None


rows = []
regressions = []
unavailable = []
for pair in sorted(pairs):
    before = record(captures["before"], pair)
    after = record(captures["after"], pair)
    row = {"pair": pair}
    for side, values in (("before", before), ("after", after)):
        row[side] = {
            "compared": values["compared"],
            "resource_limit_failure": values["resource_limit_failure"],
            "limit_scale_used": values["limit_scale_used"],
            "coverage_old": values["coverage_old"],
            "coverage_new": values["coverage_new"],
            "unresolved_regions": values["unresolved_regions"],
            "changed_token_precision": metric(values, "scoped_token_metrics", "precision"),
            "false_positive_tokens_per_10k_unchanged": metric(
                values, "scoped_token_metrics", "false_positive_tokens_per_10k_unchanged"
            ),
            "scoped_event_recall": metric(values, "scoped_event_metrics", "recall"),
            "reviewed_recall_metrics": values.get("reviewed_recall_metrics"),
            "expected_change_failures": metric(values, "expected_change_diagnostics", "failures"),
        }
    if before["compared"] and not after["compared"]:
        regressions.append({"pair": pair, "metric": "comparison_execution"})
    for name in ("coverage_old", "coverage_new"):
        old_coverage = before.get(name)
        new_coverage = after.get(name)
        if old_coverage is not None and (new_coverage is None or new_coverage < old_coverage):
            regressions.append({"pair": pair, "metric": name, "before": old_coverage, "after": new_coverage})
    for name, old_metric in (before.get("reviewed_recall_metrics") or {}).items():
        old_recall = old_metric.get("recall")
        new_recall = (after.get("reviewed_recall_metrics") or {}).get(name, {}).get("recall")
        if old_recall is not None and (new_recall is None or new_recall < old_recall):
            regressions.append({
                "pair": pair,
                "metric": name,
                "before": old_recall,
                "after": new_recall,
            })
    for name, higher_is_better in (
        ("changed_token_precision", True),
        ("false_positive_tokens_per_10k_unchanged", False),
    ):
        old, new = row["before"][name], row["after"][name]
        if old is None or new is None:
            unavailable.append({"pair": pair, "metric": name})
            if old is not None:
                regressions.append({"pair": pair, "metric": name, "reason": "metric_became_unavailable"})
        elif (new < old if higher_is_better else new > old):
            regressions.append({"pair": pair, "metric": name, "before": old, "after": new})
    rows.append(row)

result = {
    "issue_resolved": False,
    "pair_count": len(rows),
    "capture_paths": {side: str(path.relative_to(root)) for side, path in paths.items()},
    "capture_sha256": {
        side: hashlib.sha256(path.read_bytes()).hexdigest() for side, path in paths.items()
    },
    "regressions": regressions,
    "unavailable_quality_metrics": unavailable,
    "records": rows,
}
(root / args.output).write_text(json.dumps(result, indent=2) + "\n")
print(f"{len(rows)} pairs; {len(regressions)} regressions; {len(unavailable)} unavailable metrics")
if regressions:
    raise SystemExit(1)
