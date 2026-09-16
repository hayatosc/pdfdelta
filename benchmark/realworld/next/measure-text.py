#!/usr/bin/env python3
"""Run the test-only text retrieval experiment in isolated release processes."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new directory for logs and results")
    parser.add_argument("--repetitions", type=int, default=3)
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    root = Path(__file__).resolve().parents[3]
    manifest_path = root / "benchmark/realworld/next/text-matrix.json"
    matrix = json.loads(manifest_path.read_text())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    build = subprocess.run(
        ["cargo", "test", "--release", "-p", "pdfdelta-core", "--lib", "--no-run",
         "--message-format=json"],
        cwd=root, text=True, stdout=subprocess.PIPE, check=True,
    )
    artifacts = [json.loads(line) for line in build.stdout.splitlines()]
    binaries = [item["executable"] for item in artifacts
                if item.get("reason") == "compiler-artifact"
                and item.get("profile", {}).get("test") and item.get("executable")]
    if len(binaries) != 1:
        raise RuntimeError(f"expected one core test executable, got {binaries}")
    binary = Path(binaries[0])
    tracked = [manifest_path, Path(__file__),
               root / "crates/pdfdelta-core/src/document/text_candidates.rs",
               root / "crates/pdfdelta-core/src/document/text_candidates/measurement.rs",
               root / "crates/pdfdelta-core/src/document/matching.rs",
               root / "Cargo.lock"]
    metadata = {
        "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "source_sha256": {str(path.relative_to(root)): digest(path) for path in tracked},
        "binary_sha256": digest(binary),
        "platform": platform.platform(),
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
        "repetitions": args.repetitions,
        "memory_unit": "KiB; GNU time maximum resident set of each isolated test process; not allocator live heap",
        "time_unit": "seconds; kernel includes synthetic graph construction, eligible text features, enumeration, digest and solver; excludes PDF I/O and base identity proposals; process includes test startup and JSON output",
    }
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    runs = []
    for case in matrix["cases"]:
        for repetition in range(args.repetitions):
            modes = ("dense", "indexed") if repetition % 2 == 0 else ("indexed", "dense")
            for mode in modes:
                name = f'{case["id"]}-{mode}-{repetition + 1}'
                timing = output / f"{name}.time"
                env = dict(os.environ, PDFDELTA_TEXT_CASE=case["id"], PDFDELTA_TEXT_MODE=mode)
                process = subprocess.run(
                    ["/usr/bin/time", "-f", "%e %M", "-o", str(timing), str(binary),
                     "document::text_candidates::measurement::measure_text_retrieval",
                     "--exact", "--ignored", "--nocapture", "--test-threads=1"],
                    env=env, cwd=root, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                )
                (output / f"{name}.log").write_text(process.stdout)
                records = [json.loads(line.split("TEXT_MEASUREMENT ", 1)[1])
                           for line in process.stdout.splitlines() if "TEXT_MEASUREMENT " in line]
                record = {"case": case["id"], "mode": mode, "repetition": repetition + 1,
                          "exit_code": process.returncode, "measurement": records[0] if len(records) == 1 else None}
                if process.returncode == 0:
                    elapsed, rss = timing.read_text().split()
                    record.update(process_seconds=float(elapsed), peak_rss_kib=int(rss))
                runs.append(record)
                (output / "runs.json").write_text(json.dumps(runs, indent=2) + "\n")
                print(name, "exit", process.returncode, flush=True)
    summary = []
    for case in matrix["cases"]:
        for mode in ("dense", "indexed"):
            measured = [run for run in runs if run["case"] == case["id"] and run["mode"] == mode
                        and run["exit_code"] == 0 and run["measurement"] is not None]
            row = {"case": case["id"], "mode": mode, "attempts": args.repetitions,
                   "measured": len(measured),
                   "complete": sum(run["measurement"]["complete"] for run in measured)}
            if measured:
                row.update(
                    median_kernel_seconds=statistics.median(run["measurement"]["kernel_seconds"] for run in measured),
                    median_process_seconds=statistics.median(run["process_seconds"] for run in measured),
                    max_peak_rss_kib=max(run["peak_rss_kib"] for run in measured),
                )
                for key in ("candidate_checks", "token_visits", "assignment_work", "pricing_rounds", "optimization_runs"):
                    row[key] = [run["measurement"][key] for run in measured]
                row["retrieval_complete"] = sum(run["measurement"]["retrieval_complete"] for run in measured)
            summary.append(row)
        completed = [run["measurement"] for run in runs if run["case"] == case["id"]
                     and run["measurement"] and run["measurement"]["retrieval_complete"]]
        if completed and any((r["candidate_sha256"], r["retained_proposals"]) != (completed[0]["candidate_sha256"], completed[0]["retained_proposals"]) for r in completed):
            raise RuntimeError(f'completed certificates disagree: {case["id"]}')
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    if any(run["exit_code"] != 0 or run["measurement"] is None for run in runs):
        raise RuntimeError("measurement failures retained in runs.json")


if __name__ == "__main__":
    main()
