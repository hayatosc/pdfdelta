"""Locate frozen prose targets in retained glyph, tag, and MCID evidence.

This is a source diagnosis, not a recovery observation or an order certificate.
Run from the repository root; stdout is the reproducible JSON artifact.
"""

import hashlib
import json
from collections import Counter
from pathlib import Path


def read_reference(reference):
    data = Path(reference["path"]).read_bytes()
    if hashlib.sha256(data).hexdigest() != reference["sha256"]:
        raise ValueError(f"changed evidence: {reference['path']}")
    return json.loads(data)


def reference(path):
    return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def diagnose(export, target, resolution, side):
    evidence = export[side]
    items = evidence["native"]["items"]
    glyphs = {glyph["id"]: glyph for glyph in items}
    if len(glyphs) != len(items):
        raise ValueError("duplicate native glyph identity")
    if evidence["summary"]["revision"] != resolution[side]["sha256"]:
        raise ValueError("source revision differs from the frozen target")
    selectors = {selector["id"]: selector for selector in resolution["selectors"]}
    targets = []
    for identifier in target["core_selectors"][side]:
        selector = selectors[identifier]
        rows = (
            [row["atoms"] for row in selector["sources"]]
            if "sources" in selector
            else [row[2] for row in selector["source_rows"]]
        )
        atoms = [atom for row in rows for atom in row]
        if any(atom["kind"] != "glyph" for atom in atoms):
            raise ValueError("target includes non-native atoms")
        ids = {atom["id"] for atom in atoms}
        pages = sorted({glyphs[identifier]["page"] for identifier in ids})
        overlaps = []
        covered = set()
        for element in evidence["structured"]:
            membership = set(element["value"].get("glyphs", []))
            overlap = ids & membership
            if overlap:
                covered.update(overlap)
                overlaps.append({
                    "element": element["id"],
                    "object": element["object"],
                    "target_glyphs": len(overlap),
                    "membership_glyphs": len(membership),
                    "membership_pages": sorted({glyphs[g]["page"] for g in membership}),
                })
        marks = []
        marked = set()
        for mark in evidence["native"]["marked_content"]:
            interval = mark["glyph_range"]
            start, end = interval["start"], interval["end"]
            if not 0 <= start <= end <= len(items):
                raise ValueError("invalid marked-content range")
            overlap = ids & {glyph["id"] for glyph in items[start:end]}
            if overlap:
                marked.update(overlap)
                marks.append({**mark, "target_glyphs": len(overlap)})
        targets.append({
            "selector": identifier,
            "source_atom_occurrences": len(atoms),
            "unique_glyphs": len(ids),
            "pages": pages,
            "retained_tag_covered_glyphs": len(covered),
            "retained_tag_overlaps": overlaps,
            "marked_content_covered_glyphs": len(marked),
            "marked_content_overlaps": marks,
        })
    issues = [issue for issue in evidence["summary"]["issues"] if issue["channel"] == "relations"]
    return {
        "relation_issue_counts": dict(sorted(Counter(issue["reason"] for issue in issues).items())),
        "relation_inventories": [
            {"page": inventory["page"], "backend": inventory["backend"],
             "complete": inventory["complete"], "source_count": len(inventory["sources"])}
            for inventory in evidence["inventories"] if inventory["channel"] == "relations"
        ],
        "targets": targets,
    }


def main():
    root = Path("benchmark/realworld")
    profile_ref = reference(root / "source-boundaries/segment-profile-diagnosis.json")
    target_ref = reference(root / "followup/targets.json")
    profile = read_reference(profile_ref)
    targets = {target["pair"]: target for target in read_reference(target_ref)["targets"]}
    records = []
    for record in profile["records"]:
        target = targets[record["pair"]]
        resolution_ref = target["references"]["resolution"]
        export = read_reference(record["source_export"])
        resolution = read_reference(resolution_ref)
        records.append({
            "pair": record["pair"],
            "source_export": record["source_export"],
            "resolution": resolution_ref,
            "sides": {side: diagnose(export, target, resolution, side) for side in ("old", "new")},
        })
    print(json.dumps({
        "version": 1,
        "scope": "Frozen target coverage in previously exposed native exports; no new recovery or transition acceptance.",
        "profile_diagnosis": profile_ref,
        "targets": target_ref,
        "reproducer": reference(Path(__file__)),
        "decision": "Do not relax relation completeness: the exports also retain actual binding failures. EDPB's new frozen target spans pages 4 and 5; all 656 glyphs occur in complete marked-content ranges, but only four survive as tag memberships. Acquire the ordered mixed structure children and their unresolved dependencies before attempting a local transition certificate. FAA and NIST frozen target sides each occupy one page, so unrelated cross-page tags do not explain their target misses.",
        "records": records,
    }, indent=2))


if __name__ == "__main__":
    main()
