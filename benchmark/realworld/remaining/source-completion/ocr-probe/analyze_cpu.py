"""Summarize known fixture transcription, without certifying natural inventory."""
import hashlib
import json
from pathlib import Path

BASE = Path(__file__).resolve().parent
ROOT = BASE.parents[4]
CACHE = ROOT / "benchmark/realworld/cache/source-completion/ocr-probe"
report = json.loads((BASE / "cpu-results.json").read_text())
expected = json.loads((CACHE / "expected-pages.json").read_text())

def distance(a, b):
    row = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        previous, row[0] = row[0], i
        for j, cb in enumerate(b, 1):
            previous, row[j] = row[j], min(row[j] + 1, row[j - 1] + 1, previous + (ca != cb))
    return row[-1]

quality = []
for language in ("ja", "en"):
    observed = json.loads((CACHE / f"cpu-v1/{language}-150-t1-r1.json").read_text())
    texts = [line["text"] for line in observed["lines"]]
    target = "\n".join(expected[language])
    actual = "\n".join(texts)
    strip_space = lambda s: "".join(c for c in s if not c.isspace())
    quality.append({"language": language, "expected_lines": len(expected[language]), "detected_regions": len(texts),
        "exact_line_matches": sum(t in texts for t in expected[language]),
        "line_matches_ignoring_whitespace_for_diagnosis_only": sum(strip_space(t) in [strip_space(s) for s in texts] for t in expected[language]),
        "reference_characters": len(target), "raw_character_edit_distance": distance(target, actual),
        "non_whitespace_character_edit_distance": distance(strip_space(target), strip_space(actual)),
        "reference": expected[language], "recognized": texts,
        "inventory_complete": False})
for summary in report["summary"]:
    rows = [r for r in report["records"] if r["case"] == summary["case"] and r["threads"] == summary["threads"] and r["exit_code"] == 0]
    summary["minimum_seconds"] = min(r["total_seconds"] for r in rows)
    summary["maximum_seconds"] = max(r["total_seconds"] for r in rows)
    summary["median_load_seconds"] = sorted(r["load_seconds"] for r in rows)[len(rows)//2]
    outputs = [(ROOT / r["raw_output"]).read_bytes() for r in rows]
    summary["transcriptions_equal_across_repeats"] = len({json.dumps([x["text"] for x in json.loads(output)["lines"]]) for output in outputs}) == 1
    summary["raw_sha256"] = [hashlib.sha256(output).hexdigest() for output in outputs]
report["quality"] = quality
report["source_sha256"] = {name: hashlib.sha256((BASE / name).read_bytes()).hexdigest() for name in ["src/main.rs", "Cargo.toml", "Cargo.lock", "cpu_benchmark.py", "analyze_cpu.py"]}
(BASE / "cpu-analysis.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
print(json.dumps({"summary": report["summary"], "quality": [{k:v for k,v in q.items() if k not in ("reference","recognized")} for q in quality]},ensure_ascii=False,indent=2))
