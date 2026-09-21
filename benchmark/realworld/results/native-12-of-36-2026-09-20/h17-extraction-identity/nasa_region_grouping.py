#!/usr/bin/env python3
"""NASA 29-region causal grouping (corrected, schema-driven).

Streams only the target arrays with ijson.items. Completion flags come from
the report's real JSON paths (`extraction.old_complete`,
`extraction.new_complete`) plus the capture summary's
`comparison_complete`; no byte grep and no hardcoded flags. Relations are
compacted as yielded. Block order is preserved (never sorted). A region joins
a relation only when the ordered new_span block tuples are equal AND the
comparable ranges overlap or are exactly equal. Asserts 29 regions with
unique, non-overlapping block/range coverage totalling 47,979 comparable
tokens.
"""
import gzip, hashlib, json, sys
from collections import Counter
from pathlib import Path

import ijson


def ranges_overlap(a, b):
    return a[0] < b[1] and b[0] < a[1]


def main():
    report_path, summary_path, out_path = sys.argv[1:4]
    with gzip.open(report_path, "rb") as stream:
        old_complete = next(ijson.items(stream, "extraction.old_complete"), None)
    with gzip.open(report_path, "rb") as stream:
        new_complete = next(ijson.items(stream, "extraction.new_complete"), None)
    comparison_complete = json.loads(Path(summary_path).read_text())
    comparison_complete = next(
        (row.get("comparison_complete") for row in comparison_complete["rows"]
         if row["pair"] == "nasa-buckling-8007-1968-to-2020"), None
    )
    assert old_complete is not None and new_complete is not None, "extraction flags missing"
    assert comparison_complete is not None, "comparison_complete missing"

    with gzip.open(report_path, "rb") as stream:
        records = list(ijson.items(stream, "unresolved_regions.item"))
    compact_relations = []
    with gzip.open(report_path, "rb") as stream:
        for index, relation in enumerate(ijson.items(stream, "assessment.relations.item")):
            span = relation.get("new_span") or {}
            compact_relations.append({
                "index": index,
                "parent": relation.get("parent"),
                "outcome": relation.get("outcome"),
                "search": relation.get("search"),
                "reasons": relation.get("reasons") or [],
                "assumptions": relation.get("assumptions") or [],
                "blocks": list(span.get("blocks") or []),
                "comparable_range": span.get("comparable_range"),
                "canonical_range": span.get("canonical_range"),
            })

    compact_records = []
    tokens_total = 0
    covered = []
    for region in records:
        span = region.get("new_span") or {}
        blocks = tuple(span.get("blocks") or [])
        comparable = span.get("comparable_range") or {"start": 0, "end": 0}
        length = comparable["end"] - comparable["start"]
        tokens_total += length
        covered.append((span.get("pages"), blocks, comparable))
        matched = []
        for relation in compact_relations:
            if tuple(relation["blocks"]) != blocks:
                continue
            if not relation["comparable_range"]:
                continue
            if ranges_overlap(
                (comparable["start"], comparable["end"]),
                (relation["comparable_range"]["start"], relation["comparable_range"]["end"]),
            ) or comparable == relation["comparable_range"]:
                matched.append({
                    "index": relation["index"],
                    "parent": relation["parent"],
                    "outcome": relation["outcome"],
                    "search": relation["search"],
                    "reasons": relation["reasons"],
                    "assumptions": relation["assumptions"],
                    "comparable_range": relation["comparable_range"],
                    "exact_range": comparable == relation["comparable_range"],
                })
        compact_records.append({
            "pages": span.get("pages"),
            "blocks": list(blocks),
            "comparable_range": comparable,
            "canonical_range": span.get("canonical_range"),
            "tokens": length,
            "evidence": list(region.get("evidence") or []),
            "text_prefix": (span.get("text") or "")[:60],
            "matched_relations": matched,
        })

    assert len(compact_records) == 29, len(compact_records)
    assert tokens_total == 47979, tokens_total
    seen = Counter()
    for _pages, blocks, comparable in covered:
        seen[(blocks, comparable["start"], comparable["end"])] += 1
    assert all(count == 1 for count in seen.values()), "duplicate block/range coverage"

    reason_freq = Counter(r for rec in compact_records for m in rec["matched_relations"] for r in m["reasons"])
    assumption_freq = Counter(a for rec in compact_records for m in rec["matched_relations"] for a in m["assumptions"])
    outcome_freq = Counter(m["outcome"] for rec in compact_records for m in rec["matched_relations"])
    exact_ranges = sum(1 for rec in compact_records for m in rec["matched_relations"] if m["exact_range"])
    result = {
        "regions": len(compact_records),
        "tokens_total": tokens_total,
        "extraction": {"old_complete": old_complete, "new_complete": new_complete},
        "comparison_complete": comparison_complete,
        "matched_relation_count": sum(len(rec["matched_relations"]) for rec in compact_records),
        "exact_range_matches": exact_ranges,
        "matched_reason_frequency": dict(reason_freq.most_common()),
        "matched_assumption_frequency": dict(assumption_freq.most_common()),
        "matched_outcome_frequency": dict(outcome_freq.most_common()),
        "records": compact_records,
        "binding": {
            "report_sha256": hashlib.file_digest(Path(report_path).open("rb"), "sha256").hexdigest(),
            "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        },
    }
    Path(out_path).write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k != "records"}, indent=1)[:1800])


if __name__ == "__main__":
    main()
