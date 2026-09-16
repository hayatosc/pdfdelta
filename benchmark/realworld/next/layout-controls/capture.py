#!/usr/bin/env python3
"""Capture the registered layout controls without changing their source gold."""

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
    parser.add_argument("binary", type=Path)
    parser.add_argument("cache", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--implementation", required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    manifest = json.loads((root / "manifest.json").read_text())
    pairs = manifest["generated_pairs"]
    for pair in pairs:
        for side in ("old", "new"):
            pair[side]["file"] = str(args.cache / pair[side]["file"])
    mutations = json.loads((root / "source-mutation-expectations.json").read_text())["pairs"]
    for pair in mutations:
        if pair["kind"] != "reverse":
            for side in ("old", "new"):
                pair[side]["file"] = str(args.cache / Path(pair[side]["file"]).name)
    pairs += mutations
    for pair in pairs:
        for side in ("old", "new"):
            if sha256(Path(pair[side]["file"])) != pair[side]["sha256"]:
                parser.error(f"input hash mismatch: {pair['id']} {side}")
        annotation = root / "annotations" / f"{pair['id']}.json"
        resolved = json.loads(annotation.with_suffix(".resolved.json").read_text())
        if not resolved["selector_resolution_complete"] or sha256(annotation) != resolved["annotation_sha256"]:
            parser.error(f"unresolved or modified annotation: {pair['id']}")
    args.output.mkdir(parents=True, exist_ok=False)
    records = {
        "version": 1,
        "implementation_commit": args.implementation,
        "binary_sha256": sha256(args.binary),
        "timeout_seconds": 180,
        "limit_scale": 1,
        "reference_hashes": {name: sha256(root / name) for name in (
            "manifest.json", "expectations.json", "source-mutation-expectations.json",
        )},
        "runs": [],
    }
    routes = {
        "native": ["--native-text-only"],
        "text": ["--channels", "text"],
        "all": ["--channels", "text,visual,forms,relations"],
    }
    for pair in pairs:
        for route, flags in routes.items():
            stem = args.output / f"{pair['id']}-{route}"
            report = stem.with_suffix(".json")
            command = [str(args.binary.resolve()), pair["old"]["file"], pair["new"]["file"],
                       *flags, "--limit-scale", "1", "--quiet", "--json", str(report)]
            with stem.with_suffix(".stdout").open("wb") as out, stem.with_suffix(".stderr").open("wb") as err:
                result = subprocess.run(
                    ["/usr/bin/time", "-f", "%e %M", "-o", str(stem.with_suffix(".time")),
                     "timeout", "--kill-after=5", "180", *command], stdout=out, stderr=err, check=False,
                )
            elapsed, rss = stem.with_suffix(".time").read_text().splitlines()[-1].split()
            record = {
                "pair": pair["id"], "route": route, "exit_code": result.returncode,
                "old_sha256": pair["old"]["sha256"], "new_sha256": pair["new"]["sha256"],
                "wall_seconds": float(elapsed), "peak_rss_kib": int(rss),
                "status": "captured" if result.returncode in (0, 1, 3) and report.is_file() else "failed",
                "report_bytes": report.stat().st_size if report.is_file() else None,
                "report_sha256": sha256(report) if report.is_file() else None,
                "stderr": stem.with_suffix(".stderr").read_text()[:4096],
            }
            records["runs"].append(record)
            (args.output / "runs.json").write_text(json.dumps(records, indent=2) + "\n")
        print(pair["id"], flush=True)


if __name__ == "__main__":
    main()
