#!/usr/bin/env python3
"""Measure retained paint bounds strictly outside the native page rectangle.

This is an applicability observation, not a visibility or inventory certificate.
Unknown bounds and boundary contact remain unresolved. No production predicate
changes. The retained projection includes all paint records and inventory issues.
"""
import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import time


def reference(path):
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": str(path), "sha256": digest}


def classify(bounds, page):
    if bounds is None or page is None or not all(math.isfinite(v) for v in bounds):
        return "unknown"
    x0, y0, x1, y1 = bounds
    if x0 > x1 or y0 > y1:
        return "unknown"
    if (x1 < page["min"]["x"] or x0 > page["max"]["x"]
            or y1 < page["min"]["y"] or y0 > page["max"]["y"]):
        return "outside"
    return "contact_or_overlap"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(exist_ok=False, parents=True)
    panel = Path("benchmark/realworld/followup/panel.json")
    header = {"job": {"kind": "content", "structure": False, "first_structure_id": 0},
              "password": None, "font_identities": [], "cache_dir": None}
    result = {"scope": __doc__, "binary": reference(args.binary), "panel": reference(panel),
              "request": header, "timeout_seconds": 35, "rows": [], "status": "running"}
    page = {"min": {"x": 0, "y": 0}, "max": {"x": 10, "y": 10}}
    assert [classify(b, page) for b in (None, [-2, 1, -1, 2], [-1, 1, 0, 2],
            [0, 0, 1, 1], [11, 0, 12, 1], [0, 0, float("nan"), 1])] == [
                "unknown", "outside", "contact_or_overlap", "contact_or_overlap", "outside", "unknown"]
    for pair in json.loads(panel.read_text())["pairs"]:
        for side in ("old", "new"):
            entry = pair[side]
            source = Path(entry["path"])
            row = {"pair": pair["id"], "side": side, "input": reference(source)}
            assert row["input"]["sha256"] == entry["sha256"]
            started = time.monotonic()
            try:
                run = subprocess.run([str(args.binary.resolve()), "acquire-native"],
                    input=json.dumps(header).encode() + b"\n" + source.read_bytes(),
                    capture_output=True, timeout=35, check=False, env=os.environ)
                row.update(exit_code=run.returncode, response_sha256=hashlib.sha256(run.stdout).hexdigest())
                if run.returncode == 0:
                    reply = json.loads(run.stdout)
                    if "Ok" in reply:
                        assert reply["Ok"]["version"] == 8
                        store = reply["Ok"]["store"]
                        native = store["native"]
                        projection = {key: store[key] for key in ("revision", "backends", "pages", "inventories", "issues")}
                        projection.update({key: native[key] for key in ("last_non_text_paint", "non_text_paint_bounds")})
                        path = args.output / (pair["id"] + "-" + side + ".json")
                        path.write_text(json.dumps(projection, separators=(",", ":")) + "\n")
                        row["projection"] = reference(path)
                        pages = {p["page"]: p["bounds"] for p in store["pages"]}
                        counts = {int(p): Counter() for p in native["last_non_text_paint"]}
                        for p, _, bounds, _, _ in native["non_text_paint_bounds"] or []:
                            counts.setdefault(p, Counter())[classify(bounds, pages.get(p))] += 1
                        row["paint_pages"] = counts
                        row["outside_only_pages"] = [p for p, c in counts.items()
                            if c["outside"] and not c["unknown"] and not c["contact_or_overlap"]]
                        row["all_paint_outside"] = bool(counts) and len(row["outside_only_pages"]) == len(counts)
                    else:
                        row["error"] = reply
                else:
                    row["stderr"] = run.stderr.decode(errors="replace")
            except subprocess.TimeoutExpired:
                row["error"] = "native worker timeout"
            row["wall_seconds"] = time.monotonic() - started
            result["rows"].append(row)
            (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            print(pair["id"], side, row.get("outside_only_pages"), row.get("all_paint_outside"), flush=True)
    result["status"] = "finished"
    (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
