"""Diagnostic candidate enumeration only; never emits production diffs."""
import json
import re
from pathlib import Path

root = Path(__file__).resolve().parents[4]
blocks = json.loads((root / "target/issue12-trust-blocks.json").read_text())
windows = json.loads((root / "benchmark/realworld/results/issue12-subblock-investigation/global-runs.json").read_text())["results"][-1]
assert windows["length"] == 128
by_id = {side: {b["block_id"]: b for b in values} for side, values in blocks.items()}
anchors = []
for run in windows["runs"]:
    locations = {}
    for side, key in [("Old", "old"), ("New", "new")]:
        block_id, start, end = run[key]
        block = by_id[side][block_id]
        assert block["canonical"][start:end] == run["text"]
        locations[side] = {"block": block_id, "range": [start, end], "pages": block["pages"]}
    anchors.append(locations)

pattern = re.compile(r"(?P<label>[A-Za-z][A-Za-z0-9_-]*)\s*[:—–]\s*(?P<items>[A-Za-z][A-Za-z0-9_-]*(?:,\s*(?:and\s+)?[A-Za-z][A-Za-z0-9_-]*){3,31})")

def position(pages, anchor_pages):
    if not pages or not anchor_pages:
        return None
    if max(pages) < min(anchor_pages):
        return "before"
    if min(pages) > max(anchor_pages):
        return "after"
    return None

candidates = {}
for side, values in blocks.items():
    candidates[side] = []
    for block in values:
        for match in pattern.finditer(block["canonical"]):
            items = [re.sub(r"^and\s+", "", item.strip()).lower() for item in match["items"].split(",")]
            if len(items) != len(set(items)):
                continue
            candidates[side].append({"block": block["block_id"], "pages": block["pages"], "range": [match.start(), match.end()], "label": match["label"], "items": items, "source_text": match[0]})

relations = []
for old in candidates["Old"]:
    for new in candidates["New"]:
        if old["label"] != new["label"]:
            continue
        if [item for item in new["items"] if item in old["items"]] != old["items"]:
            continue
        if len(new["items"]) != len(old["items"]) + 1:
            continue
        evidence = [{"anchor": index, "old": position(old["pages"], a["Old"]["pages"]), "new": position(new["pages"], a["New"]["pages"])} for index, a in enumerate(anchors)]
        separated = all(e["old"] is not None and e["old"] == e["new"] for e in evidence) and bool(evidence)
        relations.append({"old": old, "new": new, "same_anchor_side": separated, "page_evidence": evidence})

output = {"status": "diagnostic_only_not_a_recovery_proof", "limitations": ["Case-insensitive item comparison proposes correspondence; original case differences must remain in exact diff.", "Page separation does not establish equivalence of the surrounding sentences.", "Anchor uniqueness in the source artifact counts within-block windows; arbitrary unproven cross-block orders are not covered.", "No benchmark quotes or pair id participate in candidate selection."], "candidate_counts": {side: len(values) for side, values in candidates.items()}, "anchors": anchors, "relations": relations}
Path(__file__).with_name("enumeration-candidates.json").write_text(json.dumps(output, indent=2, ensure_ascii=False) + "\n")
print({"candidate_counts": output["candidate_counts"], "relations": len(relations), "page_separated_relations": sum(r["same_anchor_side"] for r in relations)})
