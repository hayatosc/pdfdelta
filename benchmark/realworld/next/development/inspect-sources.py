#!/usr/bin/env python3
"""Capture source evidence before comparisons; retain every acquisition failure."""

import argparse
import hashlib
import json
from pathlib import Path
import resource
import subprocess


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def output_limit():
    # Raw glyph inspection is verbose. A truncated capture is a failed attempt,
    # never an empty source or proof of absence.
    resource.setrlimit(resource.RLIMIT_FSIZE, (256 * 1024**2, 256 * 1024**2))


def summarize_capture(path, view):
    """Count reported evidence, not visible-content or annotation completeness."""
    if view == "objects":
        with path.open() as stream:
            for line in stream:
                if line.startswith("pages: "):
                    return {"declared_pages": int(line.removeprefix("pages: "))}
        raise ValueError("successful object inspection omitted its page count")
    counts = dict(glyphs=0, unmapped_glyphs=0, nondefault_directions=0, extraction_issues=0)
    pages = set()
    with path.open() as stream:
        for line in stream:
            if line.startswith("extraction-issue:"):
                counts["extraction_issues"] += 1
            if not line.startswith("glyph id="):
                continue
            counts["glyphs"] += 1
            pages.add(int(line.split(" ", 4)[2].removeprefix("page=")))
            counts["unmapped_glyphs"] += " text=" not in line
            counts["nondefault_directions"] += " direction=(1,0) " not in line
    return dict(counts, pages_with_glyphs=len(pages))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pdfdelta", type=Path)
    parser.add_argument("cache", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--view", choices=("glyphs", "objects"), default="glyphs")
    args = parser.parse_args()
    binary = args.pdfdelta.resolve(strict=True)
    manifest = Path(__file__).with_name("inputs.json")
    inputs = json.loads(manifest.read_text())
    if not inputs["frozen"]:
        parser.error("source inspection requires frozen input selection")
    args.output.mkdir(parents=True, exist_ok=False)
    records = {
        "version": 1,
        "binary_sha256": sha256(binary),
        "inputs_sha256": sha256(manifest),
        "command": ["inspect", f"--{args.view}"],
        "timeout_seconds": 180,
        "output_limit_bytes": 256 * 1024**2,
        "comparison_performed": False,
        "attempts": [],
    }
    for pair in inputs["pairs"]:
        if "replacement" in pair:
            continue
        for side in ("old", "new"):
            stem = f"{pair['id']}-{side}"
            source = args.cache / f"{stem}.pdf"
            record = {"id": pair["id"], "side": side, "input_sha256": pair[side].get("sha256")}
            if not source.is_file():
                record["status"] = "missing_input"
            elif sha256(source) != record["input_sha256"]:
                record["status"] = "input_hash_mismatch"
            else:
                with (args.output / f"{stem}.{args.view}.txt").open("wb") as out, (
                    args.output / f"{stem}.stderr.txt"
                ).open("wb") as err:
                    result = subprocess.run(
                        ["/usr/bin/time", "-f", "%e %M", "-o", str(args.output / f"{stem}.time.txt"),
                         "timeout", "--kill-after=5", "180", str(binary), "inspect", f"--{args.view}", str(source)],
                        stdout=out, stderr=err, preexec_fn=output_limit, check=False,
                    )
                    status = "captured" if result.returncode == 0 else "inspection_failed"
                    if result.returncode in (124, 137):
                        status = "timeout"
                    record.update(status=status, exit_code=result.returncode)
                capture = args.output / f"{stem}.{args.view}.txt"
                record.update(output_bytes=capture.stat().st_size, output_sha256=sha256(capture))
                record["evidence_counts"] = (
                    summarize_capture(capture, args.view) if record["status"] == "captured" else None
                )
                record["stderr"] = (args.output / f"{stem}.stderr.txt").read_text()[:4096]
                measurement = (args.output / f"{stem}.time.txt").read_text().splitlines()[-1].split()
                record.update(wall_seconds=float(measurement[0]), peak_rss_kib=int(measurement[1]))
            records["attempts"].append(record)
            (args.output / "attempts.json").write_text(json.dumps(records, indent=2) + "\n")
            print(stem, record["status"], flush=True)


if __name__ == "__main__":
    main()
