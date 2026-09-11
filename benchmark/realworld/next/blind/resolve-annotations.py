#!/usr/bin/env python3
"""Resolve the registered literal ranges before running blind comparisons."""

import hashlib
import json
from pathlib import Path
import subprocess


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    base = Path(__file__).resolve().parent
    root = base.parents[3]
    binary = root / "target/debug/pdfbench"
    cache = root / "benchmark/realworld/cache/next-blind"
    ledger = base / "annotation-resolution-attempts.json"
    if ledger.exists():
        raise SystemExit("Preserve the existing resolution ledger before another run.")
    records = {
        "version": 1,
        "comparison_performed": False,
        "resolver_binary_sha256": digest(binary),
        "inputs_sha256": digest(base / "inputs.json"),
        "attempts": [],
    }
    for pair in json.loads((base / "inputs.json").read_text())["pairs"]:
        if "replacement" in pair:
            continue
        annotation = base / "annotations" / f"{pair['id']}.json"
        output = annotation.with_suffix(".resolved.json")
        if output.exists():
            raise SystemExit(f"Refusing to overwrite {output}")
        command = ["timeout", "--kill-after=5", "180", str(binary), "validate-literal-selectors", "--annotation", str(annotation)]
        for side in ("old", "new"):
            source = cache / f"{pair['id']}-{side}.pdf"
            if digest(source) != pair[side]["sha256"]:
                raise SystemExit(f"Input hash mismatch: {source}")
            command.extend([f"--{side}", str(source)])
        with output.open("wb") as stdout:
            result = subprocess.run(command, stdout=stdout, stderr=subprocess.PIPE, check=False)
        record = {
            "id": pair["id"],
            "annotation_sha256": digest(annotation),
            "resolution_sha256": digest(output),
            "exit_code": result.returncode,
            "stderr": result.stderr.decode(errors="replace"),
        }
        records["attempts"].append(record)
        ledger.write_text(json.dumps(records, indent=2) + "\n")
        print(pair["id"], result.returncode, flush=True)


if __name__ == "__main__":
    main()
