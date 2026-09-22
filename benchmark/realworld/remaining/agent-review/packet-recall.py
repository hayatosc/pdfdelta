#!/usr/bin/env python3
"""Packet-arm recall for every pair, computed without a reader.

A settled review can only report what the comparison examined, so its recall
against a sentence-level diff of both documents is a property of the bundle.
"""
import json, re, subprocess, sys, difflib
from pathlib import Path
import tiktoken
RUNS = Path(sys.argv[1]); PD = "/home/user/pdfdelta/target/release/pdfdelta"
enc = tiktoken.get_encoding("cl100k_base")
tok = lambda t: len(enc.encode(t, disallowed_special=()))
norm = lambda t: re.sub(r"\s+", " ", t).strip()

def sentences(path):
    return [s for s in re.split(r"(?<=[.!?])\s+", norm(path.read_text("utf-8", "replace")))
            if len(s) >= 40]

out = []
for d in sorted(p for p in RUNS.iterdir() if p.is_dir()):
    b, base = d / "bundle", d / "baseline"
    if not (b / "manifest.json").exists() or not (base / "old.txt").exists():
        continue
    old, new = sentences(base / "old.txt"), sentences(base / "new.txt")
    if len(old) < 20:
        continue
    changed = []
    for tag, i1, i2, _, _ in difflib.SequenceMatcher(None, old, new, autojunk=False).get_opcodes():
        if tag != "equal":
            changed += old[i1:i2]
    records = json.loads((b / "cases/index.json").read_text())["records"]
    settled = [r["case"] for r in records if r["finding"] == "difference_established"]
    quoted = []
    for case in settled:
        r = json.loads(subprocess.run([PD, "review", "show", str(b), "--case", case, "--detail",
                                       "text", "--max-output-bytes", "16384"],
                                      capture_output=True).stdout)
        quoted.append(norm((r.get("old_text") or {}).get("text", "")))
    text = " ".join(quoted)
    found = sum(1 for s in changed if s[:60] in text)
    full = sum(tok((base / s).read_text("utf-8", "replace")) for s in ("old.txt", "new.txt"))
    out.append({"pair": d.name, "settled_cases": len(settled),
                "changed_old_sentences": len(changed), "found": found,
                "recall": round(found / max(1, len(changed)), 4), "full_text_tokens": full})
    print(json.dumps(out[-1]), flush=True)
(RUNS / "packet-recall.json").write_text(json.dumps(out, indent=1) + "\n")
