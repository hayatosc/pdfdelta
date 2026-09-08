"""Evaluate identical diff controls under native and model block order."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import csv
import json
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("model_captures", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    benchmark = root / "benchmark/realworld"
    with (benchmark / "manifest.tsv").open() as source:
        rows = [row for row in csv.DictReader(
            (line for line in source if not line.startswith("#")), delimiter="\t",
        ) if row["expected_file"] != "-"]
    args.output.mkdir(parents=True, exist_ok=False)

    def run(row):
        pair = row["pair_id"]
        captures = [args.model_captures / f"{pair}-{side}.json"
                    for side in ("old", "new")]
        wait_started = time.monotonic()
        while not all(path.exists() for path in captures):
            failed = any(
                (args.model_captures / f"{pair}-{side}.process.json").exists()
                and not path.exists()
                for side, path in zip(("old", "new"), captures, strict=True)
            )
            if failed or time.monotonic() - wait_started > 3600:
                record = {"pair": pair, "status": "model_capture_unavailable"}
                (args.output / f"{pair}.process.json").write_text(
                    json.dumps(record, indent=2) + "\n",
                )
                print(record, flush=True)
                return
            time.sleep(10)
        command = [str(args.binary.resolve()),
                   *(str(benchmark / "cache" / f"{pair}-{side}.pdf")
                     for side in ("old", "new")),
                   str(benchmark / row["expected_file"]),
                   *(str(path) for path in captures),
                   "--limit-scale", row["limit_scale_hint"]]
        started = time.monotonic()
        with (args.output / f"{pair}.json").open("x") as output, \
                (args.output / f"{pair}.log").open("x") as log:
            try:
                result = subprocess.run(command, stdout=output, stderr=log, timeout=900)
                status = {"exit_code": result.returncode}
            except subprocess.TimeoutExpired:
                status = {"status": "external_timeout"}
        record = {"pair": pair, "seconds": time.monotonic() - started,
                  "timeout_seconds": 900, "command": command, **status}
        (args.output / f"{pair}.process.json").write_text(
            json.dumps(record, indent=2) + "\n",
        )
        print(f"{pair}: {status}", flush=True)

    with ThreadPoolExecutor(max_workers=2) as pool:
        list(pool.map(run, rows))


if __name__ == "__main__":
    main()
