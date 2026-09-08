"""Summarize order controls against the frozen, source-resolved annotations."""

import argparse
from collections import Counter
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("controls", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    experiment = Path(__file__).resolve().parent / "results/issue20-order-experiment"
    baseline = json.loads((experiment / "baseline-fixed/annotated-summary.json").read_text())
    records = []
    totals = {name: {
        "statuses": Counter(), "matched": 0, "candidate_matched": 0,
        "accepted_or_candidate_matched": 0,
        "known_scoped_fp_tokens": 0, "scoped_unmeasured_pairs": [],
        "documents_with_assessment_reason": Counter(),
    } for name in ("native_native", "model_model")}
    assignments = []
    for source in baseline["records"]:
        pair = source["pair"]
        process = json.loads((args.controls / f"{pair}.process.json").read_text())
        if process.get("exit_code") != 0:
            raise ValueError(f"Probe process did not return a report: {pair}: {process}")
        report = json.loads((args.controls / f"{pair}.json").read_text())
        record = {"pair": pair, "source_expected": source["source_expected"],
                  "source_measurable": source["measured_expected"] is not None,
                  "controls": {}}
        for side in ("old", "new"):
            assignment = report[f"{side}_assignment"]
            if assignment is not None:
                if not assignment["model_is_permutation"]:
                    raise ValueError(f"Source block preservation failed: {pair}/{side}")
                assignments.append({"pair": pair, "side": side, **{
                    key: value for key, value in assignment.items() if key != "pages"
                }})
        for control in report["controls"]:
            name = control["name"]
            total = totals[name]
            total["statuses"][control["status"]] += 1
            accepted = {item["id"]: item for item in control["expected_outcomes"] or []}
            candidates = {item["id"]: item
                          for item in control["candidate_expected_outcomes"] or []}
            expected = []
            for item in source["expectations"]:
                key = item["expected_id"]
                matched = accepted.get(key, {}).get("matched", False)
                candidate_matched = candidates.get(key, {}).get("matched", False)
                if record["source_measurable"]:
                    total["matched"] += bool(matched)
                    total["candidate_matched"] += bool(candidate_matched)
                    total["accepted_or_candidate_matched"] += bool(matched or candidate_matched)
                else:
                    matched = candidate_matched = None
                expected.append({"id": key, "matched": matched,
                                 "candidate_matched": candidate_matched,
                                 "failure_reason": accepted.get(key, {}).get("failure_reason")})
            tokens = control["reviewed_scope_tokens"]
            fp = None if tokens is None else (
                tokens["reported_changed_tokens"] - tokens["true_positive_tokens"]
            )
            if fp is None:
                total["scoped_unmeasured_pairs"].append(pair)
            else:
                total["known_scoped_fp_tokens"] += fp
            diagnostics = control.get("diagnostics") or {}
            assessment = diagnostics.get("final_assessment") or {}
            reasons = {reason for item in assessment.get("records", [])
                       for relation in item.get("relations", [])
                       for reason in relation.get("reasons", [])}
            total["documents_with_assessment_reason"].update(reasons)
            record["controls"][name] = {
                "status": control["status"], "runtime_ms": control["runtime_ms"],
                "changes": control["changes"], "candidate_changes": control["candidate_changes"],
                "old_coverage": control["old_coverage"], "new_coverage": control["new_coverage"],
                "scoped_fp_tokens": fp, "scoped_error": control["reviewed_scope_error"],
                "quality_error": control["quality_error"], "error": control["error"],
                "expectations": expected,
            }
        native = record["controls"]["native_native"]["expectations"]
        model = record["controls"]["model_model"]["expectations"]
        record["demoted_accepted_to_candidate"] = [
            old["id"] for old, new in zip(native, model, strict=True)
            if old["matched"] is True and new["matched"] is False
            and new["candidate_matched"] is True
        ]
        records.append(record)
    result = {"pairs": len(records), "source_expected": baseline["source_expected"],
              "fixed_source_measurable_expected": baseline["measured_expected"],
              "hypothesis_only": True, "totals": totals,
              "assignments": assignments, "records": records}
    with args.output.open("x") as destination:
        json.dump(result, destination, indent=2)
        destination.write("\n")
    print(json.dumps({"pairs": len(records), "totals": totals}, indent=2))


if __name__ == "__main__":
    main()
