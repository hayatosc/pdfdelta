#!/usr/bin/env python3
"""Render selected source pages with the frozen worker; never run a comparison."""

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import struct
import subprocess
import zlib


def png(width, height, rgb):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    rows = b"".join(b"\0" + rgb[y * width * 3:(y + 1) * width * 3] for y in range(height))
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("selection", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[4]
    cache = root / "benchmark/realworld/cache"
    binary = root / "target/release/pdfdelta"
    args.output.mkdir(parents=True, exist_ok=False)
    ledger = {
        "version": 1,
        "comparison_performed": False,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "selection_sha256": hashlib.sha256(args.selection.read_bytes()).hexdigest(),
        "profile": "page-rgb-white-72dpi-annotations-v1",
        "attempts": [],
    }
    for item in json.loads(args.selection.read_text()):
        stem = f"{item['id']}-{item['side']}"
        source = cache / "next-blind" / f"{stem}.pdf"
        evidence = cache / "next-blind-objects" / f"{stem}.objects.txt"
        lines = evidence.read_text().splitlines()
        page = item["page"]
        count = int(next(line.removeprefix("pages: ") for line in lines if line.startswith("pages: ")))
        line = next(line for line in lines if line.startswith(f"page {page + 1}: "))
        obj, generation = map(int, re.search(r"object (\d+):(\d+)", line).groups())
        # Dimensions are proposals from the existing object inspection. The Rust
        # worker checks page identity and exact rendered dimensions before output.
        box = re.search(r"/CropBox \[([^]]+)\]", line) or re.search(r"/MediaBox \[([^]]+)\]", line)
        bounds = list(map(float, box[1].split()))
        width, height = math.ceil(bounds[2] - bounds[0]), math.ceil(bounds[3] - bounds[1])
        rotation = re.search(r"/Rotate (-?\d+)", line)
        if rotation and int(rotation[1]) % 180:
            width, height = height, width
        metadata_hash = None
        if "native_metadata" in item:
            metadata = (root / item["native_metadata"]).read_bytes()
            metadata_hash = hashlib.sha256(metadata).hexdigest()
            native = json.loads(metadata)["Ok"]
            assert native["page_refs"][page] == {"object_number": obj, "generation": generation}
            assert native["store"]["revision"] == hashlib.sha256(source.read_bytes()).hexdigest()
            bounds = native["store"]["pages"][page]["bounds"]
            width = math.ceil(bounds["max"]["x"] - bounds["min"]["x"])
            height = math.ceil(bounds["max"]["y"] - bounds["min"]["y"])
        command = [str(binary), "render-page", *map(str, (page, count, width, height, obj, generation))]
        raw = args.output / f"{stem}-p{page}.rgb"
        with source.open("rb") as stdin, raw.open("wb") as stdout:
            result = subprocess.run(
                ["timeout", "--kill-after=2", "10", *command],
                stdin=stdin, stdout=stdout, stderr=subprocess.PIPE, check=False,
            )
        data = raw.read_bytes()
        success = result.returncode == 0 and len(data) == 1 + width * height * 3 and data[0] in (0, 1)
        record = dict(
            item,
            input_sha256=hashlib.sha256(source.read_bytes()).hexdigest(),
            object_evidence_sha256=hashlib.sha256(evidence.read_bytes()).hexdigest(),
            width=width, height=height, object_number=obj, generation=generation,
            exit_code=result.returncode,
            status="rendered" if success else "render_failed",
            stderr=result.stderr.decode(errors="replace"),
            raw_sha256=hashlib.sha256(data).hexdigest(),
        )
        if metadata_hash:
            record["native_metadata_sha256"] = metadata_hash
        if success:
            destination = raw.with_suffix(".png")
            destination.write_bytes(png(width, height, data[1:]))
            record.update(warning=bool(data[0]), png_sha256=hashlib.sha256(destination.read_bytes()).hexdigest())
        ledger["attempts"].append(record)
        (args.output / "attempts.json").write_text(json.dumps(ledger, indent=2) + "\n")
        print(stem, page, record["status"], flush=True)


if __name__ == "__main__":
    main()
