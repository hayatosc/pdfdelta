#!/usr/bin/env python3
"""What the cases in each bundle actually ask, and whether they can be answered."""
import json, sys
from collections import Counter
from pathlib import Path

RUNS = Path(sys.argv[1])
out = []
for pair_dir in sorted(p for p in RUNS.iterdir() if p.is_dir()):
    index = pair_dir / "bundle/cases/index.json"
    if not index.exists():
        continue
    records = json.loads(index.read_text())["records"]
    questions = Counter(r["question"] for r in records)
    required = Counter(
        tuple(r.get("required_evidence", [])) or ("decidable_now",) for r in records
    )
    reasons = Counter(reason for r in records for reason in r.get("reasons", []))
    answerable = sum(
        1 for r in records if not r.get("required_evidence")
    )
    out.append({
        "pair": pair_dir.name,
        "cases": len(records),
        "answerable_from_text": answerable,
        "questions": dict(questions),
        "required": {"/".join(k): v for k, v in required.items()},
        "top_reasons": dict(reasons.most_common(4)),
    })
print(json.dumps(out, indent=1))
