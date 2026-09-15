"""Measure this controlled OCR prototype sequentially, preserving raw observations."""
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[5]
CACHE = ROOT / "benchmark/realworld/cache/source-completion/ocr-probe"
PROBE = Path(__file__).resolve().parent
BINARY = PROBE / "target/release/pdfdelta-ocr-probe"
OUT = CACHE / "cpu-v1"
OUT.mkdir(exist_ok=True)
CASES = [
    ("ja-150", "ja", CACHE / "ja-page.png"),
    ("en-150", "en", CACHE / "en-page.png"),
    ("se-72", "en", ROOT / "benchmark/realworld/cache/source-completion/diagnostic-v1/se-review/old-region-0.png"),
    ("se-150", "en", CACHE / "se-150.png"),
    ("mask-72", "en", ROOT / "benchmark/realworld/cache/source-completion/partition-v1/mask-review/old-region-0.png"),
    ("mask-150", "en", CACHE / "mask-150.png"),
]

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

expected = json.loads((CACHE / "expected-pages.json").read_text())
records = []
for repeat in range(3):
    for threads in (1, 4):
        for name, language, image in CASES:
            key = f"{name}-t{threads}-r{repeat + 1}"
            output = OUT / f"{key}.json"
            timing = OUT / f"{key}.time"
            errors = OUT / f"{key}.stderr"
            command = [str(BINARY), "--page", str(CACHE / "det.onnx"), str(CACHE / f"{language}.onnx"), str(CACHE / f"{language}.dict"), str(image)]
            with output.open("w") as stdout, errors.open("w") as stderr:
                result = subprocess.run(["timeout", "60s", "/usr/bin/time", "-v", "-o", str(timing), *command], stdout=stdout, stderr=stderr, env={**os.environ, "RTEN_NUM_THREADS": str(threads)}, check=False)
            record = {"case": name, "threads": threads, "repeat": repeat + 1, "exit_code": result.returncode, "raw_output": str(output.relative_to(ROOT)), "image_sha256": digest(image)}
            if result.returncode == 0:
                report = json.loads(output.read_text())
                record.update({k: v for k, v in report.items() if k != "lines"})
                record["regions"] = len(report["lines"])
                record["nonempty_regions"] = sum(bool(line["text"]) for line in report["lines"])
                record["text_sha256"] = hashlib.sha256(json.dumps(report["lines"], ensure_ascii=False).encode()).hexdigest()
                if name in ("ja-150", "en-150"):
                    texts = [line["text"] for line in report["lines"]]
                    record["expected_lines"] = 30
                    record["exact_line_matches"] = sum(text in texts for text in expected[language])
            for line in timing.read_text().splitlines():
                if "Maximum resident set size (kbytes):" in line:
                    record["max_rss_kib"] = int(line.rsplit(":", 1)[1])
            records.append(record)
            print(key, result.returncode, record.get("total_seconds"), record.get("regions"), flush=True)
            (OUT / "records.json").write_text(json.dumps(records, indent=2) + "\n")
summary = []
for name, _, _ in CASES:
    for threads in (1, 4):
        rows = [r for r in records if r["case"] == name and r["threads"] == threads]
        ok = [r for r in rows if r["exit_code"] == 0]
        summary.append({"case": name, "threads": threads, "succeeded": len(ok), "runs": len(rows),
            "median_seconds": statistics.median(r["total_seconds"] for r in ok) if ok else None,
            "maximum_rss_kib": max((r["max_rss_kib"] for r in ok), default=None),
            "regions": [r.get("regions") for r in rows],
            "exact_line_matches": [r.get("exact_line_matches") for r in rows],
            "stable_text": len({r.get("text_sha256") for r in ok}) == 1})
report = {"binary_sha256": digest(BINARY), "models": {p.name: digest(p) for p in CACHE.iterdir() if p.suffix in (".onnx", ".dict")}, "summary": summary, "records": records, "scope": "CPU OCR prototype only; excludes PDF rendering, native extraction and solver; region decoding is approximate; no complete inventory claim"}
(PROBE / "cpu-results.json").write_text(json.dumps(report, indent=2) + "\n")
