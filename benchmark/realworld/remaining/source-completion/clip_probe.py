#!/usr/bin/env python3
"""Measure paint-record attempts under an empty execution clip.

Requires the diagnostic patch and a frozen native worker. No coverage is altered.
Failed or truncated acquisition is recorded and cannot establish complete absence.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def controls(binary, output, header):
    cases = [
        ("visible", b"1 1 2 2 re f", [False]),
        ("empty", b"0 0 10 10 re W n 20 20 10 10 re W n 1 1 2 2 re f", [True]),
        ("pending-and-restore", b"q 0 0 10 10 re W n 20 20 10 10 re W f 1 1 2 2 re f Q 1 1 2 2 re f", [False, True, False]),
    ]
    records = []
    for name, content, expected in cases:
        objects = [b"<< /Type /Catalog /Pages 2 0 R >>",
                   b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
                   b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents 4 0 R >>",
                   b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"\nendstream"]
        pdf = bytearray(b"%PDF-1.7\n")
        offsets = [0]
        for number, obj in enumerate(objects, 1):
            offsets.append(len(pdf))
            pdf.extend(str(number).encode() + b" 0 obj\n" + obj + b"\nendobj\n")
        xref = len(pdf)
        pdf.extend(b"xref\n0 5\n0000000000 65535 f \n")
        for offset in offsets[1:]:
            pdf.extend(f"{offset:010d} 00000 n \n".encode())
        pdf.extend(b"trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n" + str(xref).encode() + b"\n%%EOF\n")
        path = output / ("control-" + name + ".pdf")
        path.write_bytes(pdf)
        run = subprocess.run([str(binary.resolve()), "acquire-native"], input=header + pdf,
                             capture_output=True, timeout=35, check=False)
        actual = [line.split()[-1] == b"true" for line in run.stderr.splitlines()
                  if line.startswith(b"CLIP_PROBE ")]
        assert run.returncode == 0 and run.stdout.startswith(b'{"Ok":'), run.stderr
        assert actual == expected, (name, actual, expected)
        log = output / ("control-" + name + ".log")
        log.write_bytes(run.stderr)
        records.append({"name": name, "input_sha256": sha(path), "expected": expected,
                        "observed": actual, "log_sha256": sha(log)})
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    panel = Path("benchmark/realworld/followup/panel.json")
    results = {"binary_sha256": sha(args.binary), "panel_sha256": sha(panel),
               "completion_gain": 0, "purpose": "empty-clip applicability only", "runs": []}
    header = json.dumps({"job": {"kind": "content", "structure": False,
                                "first_structure_id": 0}, "password": None,
                         "font_identities": [], "cache_dir": None}).encode() + b"\n"
    results["controls"] = controls(args.binary, args.output, header)
    for pair in json.loads(panel.read_text())["pairs"]:
        for side in ("old", "new"):
            entry = pair[side]
            source = Path(entry["path"])
            stem = pair["id"] + "-" + side
            log = args.output / (stem + ".log")
            response = args.output / (stem + ".response.json")
            row = {"pair": pair["id"], "side": side, "input": str(source),
                   "input_sha256": sha(source)}
            assert row["input_sha256"] == entry["sha256"]
            started = time.monotonic()
            with log.open("wb") as stderr, response.open("wb") as stdout:
                try:
                    completed = subprocess.run([str(args.binary.resolve()), "acquire-native"],
                        input=header + source.read_bytes(), stdout=stdout, stderr=stderr,
                        timeout=35, check=False)
                    row["exit_code"] = completed.returncode
                except subprocess.TimeoutExpired:
                    row["exit_code"] = "timeout"
            row["wall_seconds"] = time.monotonic() - started
            with response.open("rb") as stream:
                prefix = stream.read(64)
            row["native_response_ok"] = row["exit_code"] == 0 and prefix.startswith(b'{"Ok":')
            if prefix.startswith(b'{"Err":'):
                row["native_error"] = json.loads(response.read_text())["Err"]
            row["response_sha256"] = sha(response)
            row["response_bytes"] = response.stat().st_size
            # Retain compact execution observations; the full acquisition is not
            # used as comparison evidence or as a replacement baseline.
            response.unlink()
            row["log"] = str(log)
            row["log_sha256"] = sha(log)
            total = 0
            empty = []
            for line in log.read_text(errors="replace").splitlines():
                if not line.startswith("CLIP_PROBE "):
                    continue
                _, page, stream, operator, opcode, clipped = line.split()
                total += 1
                if clipped == "true":
                    empty.append({"page": int(page), "stream": int(stream),
                                  "operator": int(operator), "opcode": opcode})
            row["observed_paints"] = total
            row["empty_clip_paints"] = empty
            results["runs"].append(row)
            (args.output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
            print(stem, row["exit_code"], total, len(empty), flush=True)


if __name__ == "__main__":
    main()
