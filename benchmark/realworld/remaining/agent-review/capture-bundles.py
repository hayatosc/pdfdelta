#!/usr/bin/env python3
"""Compare every verified pair and record the export, so retrieval can be
measured against it afterwards. Each pair runs once, with the manifest's own
registered route and limit scale."""
import csv, json, os, shutil, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
CACHE = ROOT / "benchmark/realworld/cache"
OUT = Path(sys.argv[1])
PDFDELTA = ROOT / "target/release/pdfdelta"
TIMEOUT = int(sys.argv[2]) if len(sys.argv) > 2 else 180

OUT.mkdir(parents=True, exist_ok=True)
rows = [r for r in open(ROOT / "benchmark/realworld/manifest.tsv") if not r.startswith("#")]
results = []
for r in csv.DictReader(rows, delimiter="\t"):
    pair = r["pair_id"]
    old, new = CACHE / f"{pair}-old.pdf", CACHE / f"{pair}-new.pdf"
    if not (old.exists() and new.exists()):
        continue
    bundle = OUT / pair / "bundle"
    report = OUT / pair / "report.json"
    if bundle.exists():
        shutil.rmtree(bundle)
    report.parent.mkdir(parents=True, exist_ok=True)
    if report.exists():
        report.unlink()
    args = [str(PDFDELTA), str(old), str(new), "--channels", "text",
            "--limit-scale", r["limit_scale_hint"], "--agent-review", str(bundle),
            "-j", str(report), "--quiet"]
    started = time.monotonic()
    try:
        done = subprocess.run(args, capture_output=True, timeout=TIMEOUT)
        code, err = done.returncode, done.stderr.decode()[:300]
    except subprocess.TimeoutExpired:
        code, err = None, "timeout"
    elapsed = time.monotonic() - started
    record = {
        "pair": pair, "set": r["set"], "role": r["role"],
        "in_scope": r["in_scope"] == "true", "limit_scale": float(r["limit_scale_hint"]),
        "exit": code, "seconds": round(elapsed, 1), "stderr": err,
        "input_bytes": old.stat().st_size + new.stat().st_size,
    }
    results.append(record)
    print(json.dumps(record), flush=True)

(OUT / "runs.json").write_text(json.dumps(results, indent=1) + "\n")
