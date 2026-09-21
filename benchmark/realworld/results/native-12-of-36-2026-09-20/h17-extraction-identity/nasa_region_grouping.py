#!/usr/bin/env python3
"""NASA 29-region causal grouping (schema authority: report/json.rs).

Streams ONLY the two target arrays with ijson.items (no DOM retention) and
reads completion flags from the report head. Joins regions to relations only
when the new_span block sets are EQUAL (local range numbers are not
comparable across unequal multi-block spans); flags noncomparable cases.
Emits all 29 compact region records and asserts coverage 47,979.
"""
import gzip, hashlib, json, re, sys
from collections import Counter
from pathlib import Path

import ijson


def head_keys(path, limit=2_000_000):
    with gzip.open(path, "rb") as stream:
        head = stream.read(limit)
    return {
        "new_extraction_complete": b'"new_extraction_complete"' in head,
        "old_extraction_complete": b'"old_extraction_complete"' in head,
        "new_extraction_complete_true": b'"new_extraction_complete":true' in head,
        "old_extraction_complete_true": b'"old_extraction_complete":true' in head,
    }


def main():
    report_path, out_path = sys.argv[1], sys.argv[2]
    with gzip.open(report_path, "rb") as stream:
        regions = list(ijson.items(stream, "unresolved_regions.item"))
    with gzip.open(report_path, "rb") as stream:
        relations = list(ijson.items(stream, "assessment.relations.item"))
    head = head_keys(report_path)

    def span_parts(span):
        if not span:
            return None
        return (
            tuple(sorted(span.get("blocks") or [])),
            (span.get("comparable_range") or {}).get("start"),
            (span.get("comparable_range") or {}).get("end"),
            (span.get("canonical_range") or {}).get("start"),
            (span.get("canonical_range") or {}).get("end"),
        )

    compact_relations = []
    for index, relation in enumerate(relations):
        compact_relations.append({
            "index": index,
            "parent": relation.get("parent"),
            "outcome": relation.get("outcome"),
            "search": relation.get("search"),
            "reasons": relation.get("reasons") or [],
            "assumptions": relation.get("assumptions") or [],
            "blocks": sorted((relation.get("new_span") or {}).get("blocks") or []),
            "comparable_range": (relation.get("new_span") or {}).get("comparable_range"),
        })

    records = []
    evidence_freq = Counter()
    tokens_total = 0
    noncomparable = 0
    gap = 0
    for region in regions:
        span = region.get("new_span")
        blocks = tuple(sorted((span or {}).get("blocks") or []))
        length = 0
        if span:
            length = span["comparable_range"]["end"] - span["comparable_range"]["start"]
        tokens_total += length
        labels = sorted(region.get("evidence") or [])
        for label in labels or ["<none>"]:
            evidence_freq[label] += 1
        matched = []
        for relation in compact_relations:
            if tuple(relation["blocks"]) != blocks:
                if relation["blocks"] and set(relation["blocks"]) & set(blocks):
                    noncomparable += 1
                continue
            matched.append(relation)
        if not matched:
            gap += 1
        records.append({
            "blocks": list(blocks),
            "pages": (span or {}).get("pages"),
            "comparable_range": (span or {}).get("comparable_range"),
            "canonical_range": (span or {}).get("canonical_range"),
            "tokens": length,
            "evidence": labels,
            "text_prefix": ((span or {}).get("text") or "")[:60],
            "matched_relation_indices": [r["index"] for r in matched],
            "matched_relations": matched,
        })
    assert len(records) == 29, len(records)
    assert tokens_total == 47979, tokens_total
    outcome_freq = Counter(r["outcome"] for rel in compact_relations for r in [rel] if rel["index"] in {i for rec in records for i in rec["matched_relation_indices"]})
    reason_freq = Counter(reason for rec in records for rel in rec["matched_relations"] for reason in rel["reasons"])
    assumption_freq = Counter(a for rec in records for rel in rec["matched_relations"] for a in rel["assumptions"])
    search_freq = Counter(rel["search"] for rec in records for rel in rec["matched_relations"])
    result = {
        "regions": len(records),
        "relations": len(compact_relations),
        "tokens_total": tokens_total,
        "gap_regions": gap,
        "noncomparable_overlapping_spans": noncomparable,
        "evidence_frequency": dict(evidence_freq),
        "matched_reason_frequency": dict(reason_freq.most_common()),
        "matched_assumption_frequency": dict(assumption_freq.most_common()),
        "matched_outcome_frequency": dict(outcome_freq.most_common()),
        "matched_search_frequency": dict(search_freq.most_common()),
        "head_completion_keys": head,
        "summary_comparison_complete": False,
        "records": records,
        "binding": {
            "report_sha256": hashlib.file_digest(Path(report_path).open("rb"), "sha256").hexdigest(),
            "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        },
    }
    Path(out_path).write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k != "records"}, indent=1)[:2200])


if __name__ == "__main__":
    main()
