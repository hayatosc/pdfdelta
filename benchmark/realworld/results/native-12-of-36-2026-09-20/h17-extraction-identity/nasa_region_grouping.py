#!/usr/bin/env python3
"""NASA 29-region causal grouping (schema authority: report/json.rs).

Single streaming pass with ijson.parse: collects unresolved_regions[*]
(evidence + new_span ranges/text), assessment.relations[*]
(reasons/assumptions/outcome/search/new_span) and assessment work counters.
Joins each region to relations whose new_span block sets and comparable
ranges overlap; counts gaps and overlapping relations explicitly.
"""
import gzip, hashlib, json, sys
from collections import Counter
from pathlib import Path

import ijson

TARGETS = {"unresolved_regions.item", "assessment.relations.item"}


def collect(report_path):
    regions, relations = [], []
    work = {}
    with gzip.open(report_path, "rb") as stream:
        stack, keys, targets = [], [], []
        for prefix, event, value in ijson.parse(stream):
            short = prefix.rsplit(".", 1)[-1]
            if event == "start_map":
                container = {}
                if prefix in TARGETS:
                    targets.append((container, prefix))
                if stack:
                    parent = stack[-1]
                    if isinstance(parent, dict) and keys:
                        parent[keys[-1]] = container
                    elif isinstance(parent, list):
                        parent.append(container)
                stack.append(container)
            elif event == "start_array":
                container = []
                if stack:
                    parent = stack[-1]
                    if isinstance(parent, dict) and keys:
                        parent[keys[-1]] = container
                    elif isinstance(parent, list):
                        parent.append(container)
                stack.append(container)
            elif event == "end_map":
                container = stack.pop()
                for target, name in list(targets):
                    if target is container:
                        (regions if name == "unresolved_regions.item" else relations).append(container)
                        targets.remove((target, name))
                        break
            elif event == "end_array":
                stack.pop()
            elif event == "map_key":
                keys.append(value)
            elif event in ("string", "number", "boolean", "null"):
                if prefix == "assessment.work_used":
                    work["work_used"] = value
                elif prefix == "assessment.work_limit":
                    work["work_limit"] = value
                elif prefix.startswith("assessment.work_by_stage."):
                    work.setdefault("work_by_stage", {})[short] = value
                if stack:
                    parent = stack[-1]
                    if isinstance(parent, dict) and keys:
                        parent[keys[-1]] = value
                        keys.pop()
                    elif isinstance(parent, list):
                        parent.append(value)
    return regions, relations, work


def span_range(span):
    if not span:
        return None
    return set(span.get("blocks") or []), span["comparable_range"]["start"], span["comparable_range"]["end"]


def overlaps(region_span, relation_span):
    if not region_span or not relation_span:
        return False
    blocks_a, start_a, end_a = region_span
    blocks_b, start_b, end_b = relation_span
    return bool(blocks_a & blocks_b) and start_a < end_b and start_b < end_a


def main():
    report_path, out_path = sys.argv[1], sys.argv[2]
    regions, relations, work = collect(report_path)
    evidence_freq = Counter()
    reason_freq = Counter()
    assumption_freq = Counter()
    outcome_freq = Counter()
    search_freq = Counter()
    tokens_by_evidence = Counter()
    joined = 0
    gaps = 0
    representatives = []
    for region in regions:
        span = region.get("new_span")
        evidence = tuple(sorted(region.get("evidence") or []))
        for label in evidence or ("<none>",):
            evidence_freq[label] += 1
        length = 0
        if span:
            length = span["comparable_range"]["end"] - span["comparable_range"]["start"]
        for label in evidence or ("<none>",):
            tokens_by_evidence[label] += length
        matches = [r for r in relations if overlaps(span_range(span), span_range(r.get("new_span")))]
        if not matches:
            gaps += 1
        else:
            joined += 1
            for relation in matches:
                for reason in relation.get("reasons") or []:
                    reason_freq[reason] += 1
                for assumption in relation.get("assumptions") or []:
                    assumption_freq[assumption] += 1
                outcome_freq[relation.get("outcome")] += 1
                search_freq[relation.get("search")] += 1
        if span and len(representatives) < 5 or (span and length > min((r[0] for r in representatives), default=0)):
            blocks = span.get("blocks") or []
            representatives.append((
                length,
                {
                    "blocks": blocks[:4],
                    "pages": (span.get("pages") or [])[:4],
                    "comparable_range": span["comparable_range"],
                    "text_prefix": (span.get("text") or "")[:80],
                    "evidence": list(evidence),
                    "matched_relations": len(matches),
                    "relation_reasons": sorted({r for rel in matches for r in (rel.get("reasons") or [])}),
                },
            ))
    representatives = [r for _, r in sorted(representatives, reverse=True)[:5]]
    result = {
        "regions": len(regions),
        "relations": len(relations),
        "joined_regions": joined,
        "gap_regions": gaps,
        "evidence_frequency": dict(evidence_freq.most_common()),
        "tokens_by_evidence": dict(tokens_by_evidence.most_common()),
        "relation_reason_frequency": dict(reason_freq.most_common()),
        "relation_assumption_frequency": dict(assumption_freq.most_common()),
        "relation_outcome_frequency": dict(outcome_freq.most_common()),
        "relation_search_frequency": dict(search_freq.most_common()),
        "work": work,
        "largest_representatives": representatives,
        "extraction_complete": True,
        "comparison_complete": False,
        "binding": {
            "report_sha256": hashlib.file_digest(Path(report_path).open("rb"), "sha256").hexdigest(),
            "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        },
    }
    Path(out_path).write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ("largest_representatives",)}, indent=1)[:3000])


if __name__ == "__main__":
    main()
