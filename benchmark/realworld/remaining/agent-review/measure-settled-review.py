#!/usr/bin/env python3
"""Measure a review that stops where the engine stopped.

The loop reads index pages only while they still hold cases the engine settled
something about, then opens exactly those. What it does not read is not
ignored: the remaining cases are coverage gaps, and the index says how many.
"""
import json, subprocess, sys
from pathlib import Path
import tiktoken

ROOT = Path(__file__).resolve().parents[4]
PDFDELTA = ROOT / "target/release/pdfdelta"
PDFBENCH = ROOT / "target/release/pdfbench"
RUNS = Path(sys.argv[1])
enc = tiktoken.get_encoding("cl100k_base")
tok = lambda t: len(enc.encode(t, disallowed_special=()))
run = lambda a: subprocess.run(a, capture_output=True, timeout=600).stdout.decode()

out = []
for pair_dir in sorted(p for p in RUNS.iterdir() if p.is_dir()):
    bundle = pair_dir / "bundle"
    if not (bundle / "manifest.json").exists():
        continue
    baseline = pair_dir / "baseline"
    run([str(PDFBENCH), "audit-agent-review", "--bundle", str(bundle),
         "--baseline-text", str(baseline)])
    full = sum(tok((baseline / s).read_text("utf-8", "replace"))
               for s in ("old.txt", "new.txt") if (baseline / s).exists())

    settled, cursor, index_tokens, pages, total_cases = [], None, 0, 0, 0
    while True:
        args = [str(PDFDELTA), "review", "list", str(bundle), "--max-output-bytes", "8192"]
        if cursor:
            args += ["--cursor", cursor]
        payload = run(args)
        r = json.loads(payload)
        if "error" in r:
            break
        pages += 1
        index_tokens += tok(payload)
        total_cases = r["cases_total"]
        page = r.get("cases", [])
        settled += [c["case"] for c in page if c["finding"] == "difference_established"]
        # Stop as soon as a page holds nothing the engine settled: the listing
        # is ordered so everything after it is unsettled too.
        if not any(c["finding"] == "difference_established" for c in page):
            break
        cursor = r.get("next_cursor")
        if not cursor:
            break

    show = 0
    for case in settled:
        show += tok(run([str(PDFDELTA), "review", "show", str(bundle), "--case", case,
                         "--detail", "text", "--max-output-bytes", "16384"]))
    out.append({
        "pair": pair_dir.name, "cases": total_cases, "settled": len(settled),
        "index_pages": pages, "index_tokens": index_tokens, "show_tokens": show,
        "settled_review_tokens": index_tokens + show, "full_text_tokens": full,
    })
    print(json.dumps(out[-1]), flush=True)
(RUNS / "settled.json").write_text(json.dumps(out, indent=1) + "\n")
