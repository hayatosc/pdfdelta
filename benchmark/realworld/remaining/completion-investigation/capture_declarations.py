#!/usr/bin/env python3
"""Locate source declarations in the fixed panel without certifying text coverage."""

import argparse
from collections import Counter
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import time

from capture import PANEL, reference


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    binary = args.output / "paint_trace_probe"
    shutil.copy2(args.binary, binary)
    source_paths = subprocess.check_output([
        "git", "ls-files", "--cached", "--others", "--exclude-standard", "--",
        "crates", "Cargo.toml", "Cargo.lock", ".cargo",
    ], text=True).splitlines()
    archive_path = args.output / "source.tar.gz"
    with tarfile.open(archive_path, "w:gz") as archive:
        for path in sorted(set(source_paths)):
            if Path(path).is_file():
                archive.add(path, arcname=path, recursive=False)
    shutil.copy2(__file__, args.output / "capture_declarations.py")
    helper = Path(__file__).with_name("capture.py")
    shutil.copy2(helper, args.output / "capture.py")
    result = {
        "version": 2, "certifies_text_inventory": False, "new_completions": 0,
        "panel": reference(PANEL), "binary": reference(binary),
        "source": reference(archive_path), "script": reference(__file__),
        "capture_helper": reference(helper),
        "fixed_inputs": 72, "timeout_seconds": 35,
        "scope": "Trailer-reachable dictionaries and streams declared as Forms; no execution/source binding.",
        "rows": [],
    }
    pairs = json.loads(PANEL.read_text())["pairs"]
    assert len(pairs) * 2 == result["fixed_inputs"]
    for pair in pairs:
        for side in ("old", "new"):
            source = pair[side]
            source_ref = reference(source["path"])
            if source_ref["sha256"] != source["sha256"]:
                raise ValueError(f"input hash mismatch: {pair['id']}/{side}")
            command = [str(binary), "--declarations-only", source["path"]]
            raw_path = args.output / f"{pair['id']}-{side}.json"
            started = time.monotonic()
            row = {"pair": pair["id"], "side": side, "input": source_ref, "command": command}
            try:
                with raw_path.open("xb") as stream:
                    run = subprocess.run(command, stdout=stream, stderr=subprocess.PIPE,
                                         timeout=result["timeout_seconds"], check=False)
                row.update(exit_code=run.returncode, stderr=run.stderr.decode(errors="replace"))
                row["status"] = "captured" if run.returncode == 0 else "probe_failed"
                if run.returncode == 0:
                    raw = json.loads(raw_path.read_text())
                    if raw["input_sha256"] != source["sha256"] or raw["certifies_text_inventory"]:
                        raise ValueError("probe violated the diagnostic contract")
                    meta = raw["reachable_text_declarations"]
                    row.update(
                        metadata_status=meta["status"], metadata_reason=meta["reason"],
                        visited_objects=meta["visited_objects"], charged_bytes=meta["charged_bytes"],
                        declarations_by_key=dict(Counter(d["key"] for d in meta["declarations"])),
                        forms=len(meta["form_programs"]),
                        form_inline_declarations=sum(
                            len(f["observed"]["inline_actual_text_declarations"])
                            for f in meta["form_programs"]
                        ),
                    )
            except subprocess.TimeoutExpired:
                row["status"] = "timeout"
            row["seconds"] = round(time.monotonic() - started, 3)
            row["raw"] = reference(raw_path)
            result["rows"].append(row)
            (args.output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
            print(pair["id"], side, row["status"], row.get("metadata_status"),
                  row.get("declarations_by_key"), flush=True)
    return int(any(row["status"] != "captured" for row in result["rows"]))


if __name__ == "__main__":
    raise SystemExit(main())
