"""Capture model-order hypotheses for every annotated revision pair."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import csv
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--model", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    benchmark = root / "benchmark/realworld"
    with (benchmark / "manifest.tsv").open() as source:
        rows = list(csv.DictReader(
            (line for line in source if not line.startswith("#")), delimiter="\t",
        ))
    metadata = json.loads(args.model.read_text())
    model_id = {key: metadata[key] for key in ("repo", "revision")}
    args.output.mkdir(parents=True, exist_ok=True)

    def run(name):
        pdf = benchmark / "cache" / f"{name}.pdf"
        output = args.output / f"{name}.json"
        if output.exists():
            capture = json.loads(output.read_text())
            if (capture["source_sha256"] != hashlib.sha256(pdf.read_bytes()).hexdigest()
                    or capture["model"] != model_id or capture["device"] != "cpu"):
                raise ValueError(f"Existing capture does not match inputs: {output}")
            print(f"{name}: retained verified capture", flush=True)
            return
        command = [sys.executable, str(benchmark / "issue20_model_order.py"),
                   str(pdf), str(output), "--model", str(args.model)]
        started = time.monotonic()
        with (args.output / f"{name}.log").open("x") as log:
            try:
                result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                        timeout=900)
                status = {"exit_code": result.returncode}
            except subprocess.TimeoutExpired:
                status = {"status": "external_timeout"}
        record = {"input": name, "timeout_seconds": 900,
                  "seconds": time.monotonic() - started, "command": command, **status}
        with (args.output / f"{name}.process.json").open("x") as destination:
            json.dump(record, destination, indent=2)
            destination.write("\n")
        print(f"{name}: {status}", flush=True)

    names = [f"{row['pair_id']}-{side}" for row in rows
             if row["expected_file"] != "-" for side in ("old", "new")]
    with ThreadPoolExecutor(max_workers=2) as pool:
        list(pool.map(run, names))


if __name__ == "__main__":
    main()
