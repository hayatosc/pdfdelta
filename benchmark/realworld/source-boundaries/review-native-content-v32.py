"""Check retained mixed native content against the fixed EDPB source targets.

Run from the repository root. This verifies acquisition only, not an accepted
transition: other declarations, page closure, and exact cut extents still matter.
"""

from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path("benchmark/realworld/remaining").resolve()))
import verify

verify.CONTRACT = "source-boundaries-v1"

def reference(path):
    return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def read(ref):
    data = Path(ref["path"]).read_bytes()
    assert hashlib.sha256(data).hexdigest() == ref["sha256"], ref["path"]
    return json.loads(data)


def main():
    base = Path("benchmark/realworld/source-boundaries")
    observation_ref = reference(base / "v32-acquisition-observation.json")
    observation = read(observation_ref)
    source = read(observation["sources"])
    diagnosis_ref = reference(base / "segment-target-diagnosis.json")
    diagnosis = next(row for row in read(diagnosis_ref)["records"]
                     if row["pair"] == "edpb-restrictions-v1-to-final")
    previous = read(diagnosis["source_export"])
    resolution = read(diagnosis["resolution"])
    checks = {}
    for side in ("old", "new"):
        old, new = previous[side], source[side]
        assert old["summary"]["revision"] == new["summary"]["revision"] == resolution[side]["sha256"]
        before, after = old["native"]["items"], new["native"]["items"]
        assert len(before) == len(after)
        differences = Counter()
        for a, b in zip(before, after):
            changed = {key for key in a.keys() | b.keys() if a.get(key) != b.get(key)}
            # Older exported geometry has rounding differences. Raw identities,
            # text, codes, modes, and operator/object provenance must agree exactly.
            assert changed <= {"font_size", "bbox", "baseline"}
            differences.update(changed)
        assert {key: value for key, value in old["native"].items() if key != "items"} == {
            key: value for key, value in new["native"].items() if key != "items"}
        legacy = copy.deepcopy(new["structured"])
        for element in legacy:
            element["value"].pop("content", None)
        assert legacy == old["structured"]
        checks[side] = {"glyphs": len(after), "raw_source_identity_equal": True,
                        "legacy_structure_equal": True,
                        "geometry_difference_counts": dict(sorted(differences.items()))}

    evidence = source["new"]
    elements = {element["id"]: element for element in evidence["structured"]}
    root = next(element for element in elements.values()
                if element["object"] == {"object_number": 484, "generation": 0})
    flattened, slots, unresolved = [], [], []

    def visit(element, path, ancestors):
        assert element not in ancestors
        content = elements[element]["value"]["content"]
        for offset, kid in enumerate(content):
            location = path + [[element, offset]]
            if kid["kind"] == "element":
                child = elements[kid["element"]]["value"]
                assert child["parent"] == element and child["order"] == offset
                visit(kid["element"], location, ancestors | {element})
            elif kid["kind"] == "marked_content":
                mark = evidence["native"]["marked_content"][kid["sequence"]]
                assert mark["complete"]
                interval = mark["glyph_range"]
                ids = [glyph["id"] for glyph in evidence["native"]["items"][interval["start"]:interval["end"]]]
                slots.append({"path": location, "sequence": kid["sequence"],
                              "page": mark["page"], "mcid": mark["mcid"], "glyphs": len(ids)})
                flattened.extend(ids)
            else:
                assert kid["kind"] == "unresolved"
                unresolved.append(location)

    visit(root["id"], [], set())
    counts = Counter(flattened)
    selectors = []
    for selector in resolution["selectors"]:
        if selector["id"] not in ("body-new-1", "body-new-2"):
            continue
        atoms = [atom for row in selector["source_rows"] for atom in row[2]]
        assert all(atom["kind"] == "glyph" for atom in atoms)
        ids = [atom["id"] for atom in atoms]
        assert all(counts[glyph] == 1 for glyph in ids)
        selected = set(ids)
        assert [glyph for glyph in flattened if glyph in selected] == ids
        selectors.append({"selector": selector["id"], "glyphs": len(ids),
                          "all_present_once_in_declared_order": True})
    assert len(selectors) == 2 and not unresolved
    panel_ref = reference(base / "v31-panel-observations.json")
    baseline = next(row for row in read(panel_ref)["observations"]
                    if row["pair"] == "edpb-restrictions-v1-to-final")
    before = [verify.event_digest(event) for event in verify.events(read(baseline["report"]))]
    after = [verify.event_digest(event) for event in verify.events(read(observation["report"]))]
    assert before == after
    print(json.dumps({
        "version": 1, "scope": "Acquisition proof only; no new recovery or transition acceptance.",
        "observation": observation_ref, "previous_diagnosis": diagnosis_ref,
        "previous_panel": panel_ref, "reproducer": reference(Path(__file__)),
        "source_checks": checks, "structure": root["id"], "object": root["object"],
        "ordered_slots": slots, "unresolved_slots_in_selected_subtree": unresolved,
        "target_coverage": selectors,
        "repeated_non_target_glyphs": sum(count > 1 for count in counts.values()),
        "unchanged_comparison_event_count": len(after),
        "remaining": "This subtree also declares a repeated off-target MCID. A whole-tree unique-membership requirement would still reject it. Local source-set and order closure must exclude omitted or competing declarations and prove each page region before admitting a finite target comparison.",
    }, indent=2))


if __name__ == "__main__":
    main()
