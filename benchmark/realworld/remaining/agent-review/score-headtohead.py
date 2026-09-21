#!/usr/bin/env python3
"""Score two ways of reviewing one pair against one ground truth.

Pre-registered before either arm was run. Ground truth is a sentence-level diff
of both baseline texts, the same construction used for the oecd loop record.

A changed old-side sentence counts as found by an arm when that arm's quoted
old-side material contains it (packet arm) or is contained by it (reader arm,
which quotes fragments rather than whole sentences).
"""
import json, re, sys, difflib
from pathlib import Path

RUN = Path(sys.argv[1])
READER = Path(sys.argv[2])          # JSON: {"fragments": ["...", ...]}
MIN_SENTENCE = 40
MIN_FRAGMENT = 30

norm = lambda t: re.sub(r"\s+", " ", t).strip()

def sentences(path):
    text = norm(path.read_text("utf-8", "replace"))
    return [s for s in re.split(r"(?<=[.!?])\s+", text) if len(s) >= MIN_SENTENCE]

old = sentences(RUN / "baseline/old.txt")
new = sentences(RUN / "baseline/new.txt")
matcher = difflib.SequenceMatcher(None, old, new, autojunk=False)
changed, equal = [], 0
for tag, i1, i2, j1, j2 in matcher.get_opcodes():
    if tag == "equal":
        equal += i2 - i1
    else:
        changed += old[i1:i2]

# Packet arm: what the settled cases quote.
index = json.loads((RUN / "bundle/cases/index.json").read_text())
settled = [r["case"] for r in index["records"] if r["finding"] == "difference_established"]
import subprocess
PD = "/home/user/pdfdelta/target/release/pdfdelta"
quoted = []
for case in settled:
    out = subprocess.run([PD, "review", "show", str(RUN / "bundle"), "--case", case,
                          "--detail", "text", "--max-output-bytes", "16384"],
                         capture_output=True).stdout
    r = json.loads(out)
    quoted.append(norm((r.get("old_text") or {}).get("text", "")))
packet_text = " ".join(quoted)
packet_found = [s for s in changed if s[:60] in packet_text]

# Reader arm: fragments the reader quoted from the old side.
fragments = [norm(f) for f in json.loads(READER.read_text())["fragments"]]
short = [f for f in fragments if len(f) < MIN_FRAGMENT]
reader_found, matched_fragments = [], set()
for s in changed:
    for f in fragments:
        if len(f) >= MIN_FRAGMENT and f in s:
            reader_found.append(s)
            matched_fragments.add(f)
            break
unmatched = [f for f in fragments if len(f) >= MIN_FRAGMENT and f not in matched_fragments]

result = {
    "pair": RUN.name,
    "ground_truth": {"old_sentences": len(old), "new_sentences": len(new),
                     "identical_in_position": equal, "changed_old_sentences": len(changed)},
    "packet_arm": {"settled_cases": len(settled), "found": len(packet_found),
                   "recall": round(len(packet_found) / max(1, len(changed)), 4)},
    "reader_arm": {"fragments_submitted": len(fragments),
                   "fragments_too_short_ignored": len(short),
                   "fragments_matching_no_changed_sentence": len(unmatched),
                   "found": len(reader_found),
                   "recall": round(len(reader_found) / max(1, len(changed)), 4)},
    "unmatched_fragments": unmatched,
}
print(json.dumps(result, indent=1, ensure_ascii=False))
