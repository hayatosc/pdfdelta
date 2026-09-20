#!/usr/bin/env python3
"""Render a pair-level metric delta between two native capture summaries.

Both summaries must bind the same frozen panel, denominator and route. The
output is a Markdown table of before/after totals plus a review list for rows
whose resolved coverage decreased or whose unresolved regions grew, so a
change is never classified as a gain from aggregate counts alone.
"""

import argparse
import hashlib
import json
from pathlib import Path

PANEL_SHA256 = "c3aa4a5dd7edb3b7b4b5144ea31a9eb48645fa5ebe5a27546f3be09b0dd6e744"
CONTRACT_FIELDS = ("panel", "fixed_denominator", "route", "limit_scale", "timeout_seconds")


def sha256(path):
    return hashlib.file_digest(Path(path).open("rb"), "sha256").hexdigest()


def metric(row, coverage, field):
    return row[coverage][field]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    before = json.loads(args.before.read_text())
    after = json.loads(args.after.read_text())
    for field in CONTRACT_FIELDS:
        if before[field] != after[field]:
            raise ValueError(f"capture contract differs: {field}")
    if before["panel"]["sha256"] != PANEL_SHA256:
        raise ValueError("before capture does not bind the frozen panel")
    if len(before["rows"]) != 36 or len(after["rows"]) != 36:
        raise ValueError("capture summary does not cover 36 pairs")
    previous = {row["pair"]: row for row in before["rows"]}
    current = {row["pair"]: row for row in after["rows"]}
    if previous.keys() != current.keys():
        raise ValueError("capture summaries cover different pairs")
    lines = [
        "# H2 to H5 native capture delta",
        "",
        f"- before: `{args.before}` sha256 `{sha256(args.before)}`",
        f"- after: `{args.after}` sha256 `{sha256(args.after)}`",
        f"- panel sha256 `{PANEL_SHA256}`",
        f"- route `{before['route']}`, fixed denominator `{before['fixed_denominator']}`",
        "",
        "| pair | complete b/a | old resolved b -> a | new resolved b -> a | unresolved b -> a | content b -> a | formatting b -> a | uncertain b -> a |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    review = []
    totals = {
        "old_resolved": [0, 0],
        "new_resolved": [0, 0],
        "unresolved_regions": [0, 0],
        "content_changes": [0, 0],
        "formatting_only_changes": [0, 0],
        "uncertain_changes": [0, 0],
    }
    for pair in sorted(previous):
        a = previous[pair]
        b = current[pair]
        if a["status"] != "captured" or b["status"] != "captured":
            raise ValueError(f"unhealthy capture row: {pair}")
        old_a, old_b = metric(a, "old_alignment_coverage", "resolved_tokens"), metric(
            b, "old_alignment_coverage", "resolved_tokens"
        )
        new_a, new_b = metric(a, "new_alignment_coverage", "resolved_tokens"), metric(
            b, "new_alignment_coverage", "resolved_tokens"
        )
        u_a, u_b = a["unresolved_regions"], b["unresolved_regions"]
        c_a, c_b = a["content_changes"], b["content_changes"]
        f_a, f_b = a["formatting_only_changes"], b["formatting_only_changes"]
        x_a, x_b = a["uncertain_changes"], b["uncertain_changes"]
        for key, pair_values in (
            ("old_resolved", (old_a, old_b)),
            ("new_resolved", (new_a, new_b)),
            ("unresolved_regions", (u_a, u_b)),
            ("content_changes", (c_a, c_b)),
            ("formatting_only_changes", (f_a, f_b)),
            ("uncertain_changes", (x_a, x_b)),
        ):
            totals[key][0] += pair_values[0]
            totals[key][1] += pair_values[1]
        complete = f"{int(a['comparison_complete'])}/{int(b['comparison_complete'])}"
        lines.append(
            f"| {pair} | {complete} | {old_a} -> {old_b} | {new_a} -> {new_b} | "
            f"{u_a} -> {u_b} | {c_a} -> {c_b} | {f_a} -> {f_b} | {x_a} -> {x_b} |"
        )
        if (
            old_b < old_a
            or new_b < new_a
            or u_b > u_a
            or (a["comparison_complete"] and not b["comparison_complete"])
        ):
            review.append(pair)
    lines.append(
        "| **totals** | {}/{} | {} -> {} | {} -> {} | {} -> {} | {} -> {} | {} -> {} | {} -> {} |".format(
            int(before["complete_pairs"]),
            int(after["complete_pairs"]),
            *totals["old_resolved"],
            *totals["new_resolved"],
            *totals["unresolved_regions"],
            *totals["content_changes"],
            *totals["formatting_only_changes"],
            *totals["uncertain_changes"],
        )
    )
    lines += ["", "## Rows requiring explanation", ""]
    lines.append(
        "- none"
        if not review
        else "\n".join(f"- `{pair}`" for pair in sorted(review))
    )
    args.output.write_text("\n".join(lines) + "\n")
    print(f"wrote {args.output} ({len(review)} rows require explanation)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
