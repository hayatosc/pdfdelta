#!/usr/bin/env python3
"""Download preselected inputs and retain failed attempts without replacing them."""

import hashlib
import json
from pathlib import Path
import subprocess


def main():
    root = Path(__file__).resolve().parent
    selection = root / "download-selection.json"
    cache = root.parents[1] / "cache" / "next-blind"
    cache.mkdir(parents=True, exist_ok=True)
    output = root / "download-attempts.json"
    if output.exists():
        raise SystemExit("Preserve the existing attempt ledger before another acquisition.")
    ledger = {
        "version": 1,
        "selection_sha256": hashlib.sha256(selection.read_bytes()).hexdigest(),
        "comparison_performed": False,
        "pairs": [],
    }
    for pair in json.loads(selection.read_text())["pairs"]:
        record = {"id": pair["id"]}
        for side in ("old", "new"):
            url = pair[side]
            if url is None:
                record[side] = {"status": "edition_url_unresolved"}
                continue
            target = cache / f"{pair['id']}-{side}.pdf"
            if target.exists():
                raise SystemExit(f"Refusing to overwrite {target}")
            result = subprocess.run(
                ["curl", "--location", "--fail", "--silent", "--show-error",
                 "--connect-timeout", "20", "--max-time", "90",
                 "--max-filesize", "104857600", "--output", str(target),
                 "--write-out", "%{http_code} %{url_effective}", url],
                capture_output=True, text=True, check=False,
            )
            data = target.read_bytes() if target.exists() else b""
            record[side] = {
                "url": url,
                "status": "downloaded" if result.returncode == 0 and data.startswith(b"%PDF-") else "download_failed",
                "exit_code": result.returncode,
                "http_result": result.stdout,
                "stderr": result.stderr[:4096],
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest() if data else None,
            }
            print(pair["id"], side, record[side]["status"], flush=True)
        ledger["pairs"].append(record)
        output.write_text(json.dumps(ledger, indent=2) + "\n")


if __name__ == "__main__":
    main()
