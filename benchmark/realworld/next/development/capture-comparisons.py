#!/usr/bin/env python3
"""Capture three separate routes for explicitly selected registered pairs."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pdfdelta", type=Path)
    parser.add_argument("cache", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--pair", action="append", required=True)
    parser.add_argument("--implementation", required=True)
    args = parser.parse_args()
    binary = args.pdfdelta.resolve(strict=True)
    manifest = Path(__file__).with_name("inputs.json")
    inputs = json.loads(manifest.read_text())
    if not inputs["frozen"]:
        parser.error("comparison requires frozen input selection")
    pairs = {pair["id"]: pair for pair in inputs["pairs"]}
    if len(set(args.pair)) != len(args.pair) or any(id not in pairs for id in args.pair):
        parser.error("pair IDs must be unique registered IDs")
    references = {}
    for id in args.pair:
        reference = manifest.parent / "annotations" / f"{id}.expectations.json"
        if not reference.is_file():
            parser.error(f"fix a source reference or unresolved annotation record first: {reference}")
        contract = json.loads(reference.read_text())
        if contract.get("pair") != id or contract.get("frozen_before_comparison") is not True:
            parser.error(f"source reference is not frozen for {id}")
        annotation = reference.with_name(f"{id}.json")
        if contract.get("annotation_status") != "unresolved" and (
            not annotation.is_file() or sha256(annotation) != contract.get("annotation_sha256")
        ):
            parser.error(f"source annotation hash mismatch for {id}")
        references[id] = sha256(reference)
    args.output.mkdir(parents=True, exist_ok=False)
    records = {
        "version": 1,
        "implementation_commit": args.implementation,
        "binary_sha256": sha256(binary),
        "inputs_sha256": sha256(manifest),
        "timeout_seconds": 180,
        "limit_scale": 1,
        "annotation_scoring": False,
        "provenance_note": "Caller identifies the build commit; the driver binds the executable hash but does not independently prove its build provenance.",
        "runs": [],
    }
    routes = {
        "native": ["--native-text-only"],
        "text": ["--channels", "text"],
        "all": ["--channels", "text,visual,forms,relations"],
    }
    for id in args.pair:
        pair = pairs[id]
        paths = [args.cache / f"{id}-{side}.pdf" for side in ("old", "new")]
        hashes = [pair[side].get("sha256") for side in ("old", "new")]
        acquisition_valid = all(path.is_file() and sha256(path) == hash for path, hash in zip(paths, hashes))
        for route, flags in routes.items():
            stem = args.output / f"{id}-{route}"
            record = dict(pair=id, route=route, old_sha256=hashes[0], new_sha256=hashes[1],
                          reference_sha256=references[id])
            if not acquisition_valid:
                record["status"] = "missing_input_or_hash_mismatch"
            else:
                report = stem.with_suffix(".json")
                command = [str(binary), *map(str, paths), *flags, "--limit-scale", "1",
                           "--quiet", "--json", str(report)]
                with stem.with_suffix(".stdout").open("wb") as out, stem.with_suffix(".stderr").open("wb") as err:
                    result = subprocess.run(
                        ["/usr/bin/time", "-f", "%e %M", "-o", str(stem.with_suffix(".time")),
                         "timeout", "--kill-after=5", "180", *command],
                        stdout=out, stderr=err, check=False,
                    )
                time = stem.with_suffix(".time").read_text().splitlines()[-1].split()
                record.update(exit_code=result.returncode, wall_seconds=float(time[0]), peak_rss_kib=int(time[1]),
                              status="captured" if result.returncode in (0, 1, 3) and report.is_file() else "failed")
                record.update(report_bytes=report.stat().st_size if report.is_file() else None,
                              report_sha256=sha256(report) if report.is_file() else None)
                record["stderr"] = stem.with_suffix(".stderr").read_text()[:4096]
            records["runs"].append(record)
            (args.output / "runs.json").write_text(json.dumps(records, indent=2) + "\n")
            print(id, route, record["status"], flush=True)


if __name__ == "__main__":
    main()
