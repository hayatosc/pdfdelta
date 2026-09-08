"""Capture independent revision comparisons without changing their annotations."""

import argparse
import concurrent.futures
import csv
import hashlib
import json
from pathlib import Path
import subprocess
import shutil
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--binary", type=Path, default=Path("target/release/pdfbench"))
    parser.add_argument("--manifest", type=Path, default=Path("benchmark/realworld/manifest.tsv"))
    parser.add_argument("--cache-dir", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=2)
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="pdfdelta-claims-binary-") as directory:
        snapshot = Path(directory) / "pdfbench"
        shutil.copy2(binary, snapshot)
        capture(args, snapshot)


def capture(args, binary):
    with args.manifest.open() as source:
        rows = list(csv.DictReader((line for line in source if not line.startswith("#")), delimiter="\t"))
    metadata = {
        "binary_source": str(args.binary.resolve()),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "source_sha256": {
            str(path): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted({Path("Cargo.lock"), Path("Cargo.toml"),
                                *Path("crates").rglob("Cargo.toml"),
                                *Path("crates").rglob("*.rs")})
        },
        "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
        "manifest": str(args.manifest),
        "annotation_sha256": {
            row["expected_file"]: hashlib.sha256((args.manifest.parent / row["expected_file"]).read_bytes()).hexdigest()
            for row in rows if row["expected_file"] != "-"
        },
        "jobs": args.jobs,
        "scope": "Independent PDF comparisons; unavailable quality measurements remain unavailable.",
    }
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")

    def run(row):
        pair = row["pair_id"]
        if Path(pair).name != pair or pair in (".", ".."):
            raise ValueError("manifest pair id must be a filename component")
        def artifact(suffix):
            return args.output / (pair + suffix)
        command = [str(binary), "revisions", "--manifest", str(args.manifest),
                   "--cache-dir", str(args.cache_dir), "--pair", pair,
                   "--json-output", str(artifact(".raw.json")),
                   "--summary-json-output", str(artifact(".summary.json")),
                   "--evaluation-json-output", str(artifact(".evaluation.json"))]
        timing = artifact(".time.txt")
        measured = ["/usr/bin/time", "-f", "%e\n%M", "-o", str(timing), *command]
        start = time.monotonic()
        with artifact(".log").open("w") as log:
            result = subprocess.run(measured, stdout=log, stderr=subprocess.STDOUT, check=False)
        measurement = timing.read_text().splitlines()
        record = {
            "pair": pair, "set": row["set"], "tuning_use": row["tuning_use"],
            "command": command, "exit_code": result.returncode,
            "seconds": time.monotonic() - start,
            "peak_rss_bytes": int(measurement[-1]) * 1024,
        }
        artifact(".process.json").write_text(json.dumps(record, indent=2) + "\n")
        print(f"{pair}: exit={result.returncode}, seconds={record['seconds']:.3f}", flush=True)
        return record

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        records = list(pool.map(run, rows))
    (args.output / "process-summary.json").write_text(json.dumps(records, indent=2) + "\n")


if __name__ == "__main__":
    main()
