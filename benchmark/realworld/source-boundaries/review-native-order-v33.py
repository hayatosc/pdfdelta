"""Replay native forward/inverse order acquisition on the frozen EDPB target.

This is a source-bound diagnosis, not an accepted comparison or recall result.
The target selectors locate evidence only; production discovery does not use them.
"""

from collections import Counter
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


def closed_chunks(source):
    inventory, = source["native_structures"]
    assert inventory["complete"] and inventory["root"] and inventory["parents"] is not None
    assert source["summary"]["backends"][inventory["backend"]]["kind"] == "native_parser"
    elements = {entry["id"]: entry for entry in source["structured"]
                if entry["value"]["kind"] == "structure_element"}
    assert all(entry["backend"] == inventory["backend"] for entry in elements.values())
    marks, raw = source["native"]["marked_content"], source["native"]["items"]
    owners = {entry["sequence"]: entry["owner"] for entry in inventory["parents"]}
    assert len(owners) == len(inventory["parents"])
    occurrences = Counter()
    for element in elements.values():
        for kid in element["value"]["content"]:
            assert kid["kind"] != "unresolved"
            if kid["kind"] == "marked_content":
                mark = marks[kid["sequence"]]
                assert mark["complete"]
                occurrences.update(glyph["id"] for glyph in raw[
                    mark["glyph_range"]["start"]:mark["glyph_range"]["end"]])
    chunks, visited, slots = [], set(), []
    for root_order, root in enumerate(inventory["roots"]):
        assert elements[root]["value"]["parent"] is None
        assert elements[root]["value"]["order"] == root_order
        linear = []

        def visit(identifier):
            assert identifier not in visited
            visited.add(identifier)
            element = elements[identifier]
            for offset, kid in enumerate(element["value"]["content"]):
                if kid["kind"] == "element":
                    child = elements[kid["element"]]["value"]
                    assert child["parent"] == identifier and child["order"] == offset
                    visit(kid["element"])
                elif kid["kind"] == "annotation":
                    linear.append(None)
                elif kid["kind"] == "marked_content":
                    sequence = kid["sequence"]
                    mark = marks[sequence]
                    glyphs = raw[mark["glyph_range"]["start"]:mark["glyph_range"]["end"]]
                    bound = element["object"] is not None and owners.get(sequence) == element["object"]
                    slots.append({"element": identifier, "sequence": sequence,
                                  "parent_agrees": bound, "glyphs": len(glyphs)})
                    if not glyphs:
                        linear.append(None)
                    for glyph in glyphs:
                        linear.append(glyph["id"] if bound and occurrences[glyph["id"]] == 1 else None)
                else:
                    raise AssertionError(kid)

        visit(root)
        current = []
        for glyph in linear + [None]:
            if glyph is not None:
                current.append(glyph)
            elif current:
                chunks.append(current)
                current = []
    assert visited == elements.keys()
    return chunks, occurrences, slots


def main():
    base = Path("benchmark/realworld/source-boundaries")
    observation_ref = reference(base / "v33-acquisition-observation.json")
    observation = read(observation_ref)
    source = read(observation["sources"])
    previous_ref = reference(base / "v32-acquisition-observation.json")
    previous = read(previous_ref)
    before = read(previous["sources"])
    for side in ("old", "new"):
        assert source[side]["summary"]["revision"] == before[side]["summary"]["revision"]
        assert source[side]["native"] == before[side]["native"]
        assert source[side]["inventories"] == before[side]["inventories"]
    diagnosis_ref = reference(base / "segment-target-diagnosis.json")
    diagnosis = next(row for row in read(diagnosis_ref)["records"]
                     if row["pair"] == "edpb-restrictions-v1-to-final")
    resolution = read(diagnosis["resolution"])
    chunks, counts, slots = closed_chunks(source["new"])
    selectors = [selector for selector in resolution["selectors"]
                 if selector["id"] in ("body-new-1", "body-new-2")]
    assert len(selectors) == 2
    ids = [atom["id"] for selector in selectors for row in selector["source_rows"] for atom in row[2]]
    assert len(ids) == 656 and all(counts[glyph] == 1 for glyph in ids)
    containing = [chunk for chunk in chunks if set(ids) <= set(chunk)]
    chunk, = containing
    selected = set(ids)
    assert [glyph for glyph in chunk if glyph in selected] == ids
    nodes = [node for node in source["new"]["graph"]["nodes"]
             if node["kind"] == "paragraph" and any(
                 ref.get("glyph") in selected for ref in node["sources"])]
    assert len(nodes) == 2
    node_checks = []
    for node in nodes:
        glyphs = [ref["glyph"] for ref in node["sources"] if ref["origin"] == "native"]
        assert len(glyphs) == len(node["sources"])
        supported = any(glyphs == part[start:start + len(glyphs)]
                        for part in chunks for start, glyph in enumerate(part) if glyph == glyphs[0])
        node_checks.append({"node": node["id"], "pages": node["pages"], "glyphs": len(glyphs),
                            "target_glyphs": len(set(glyphs) & selected),
                            "whole_node_in_one_parent_bound_chunk": supported})
    assert any(not node["whole_node_in_one_parent_bound_chunk"] for node in node_checks)
    first, last = chunk.index(ids[0]), chunk.index(ids[-1])
    omitted = [glyph for glyph in chunk[first:last + 1] if glyph not in selected]
    raw = {glyph["id"]: glyph for glyph in source["new"]["native"]["items"]}
    assert omitted and all(raw[glyph]["text"] == {"Mapped": " "} for glyph in omitted)
    current_events = verify.events(read(observation["report"]))
    previous_events = verify.events(read(previous["report"]))
    assert [verify.event_digest(event) for event in current_events] == [
        verify.event_digest(event) for event in previous_events]
    assert not any(event.get("source_projection", {}).get("native_regions") for event in current_events)
    print(json.dumps({
        "version": 1, "scope": "Acquisition and finite-extent diagnosis only; no new recovery.",
        "observation": observation_ref, "previous_observation": previous_ref,
        "target_diagnosis": diagnosis_ref, "reproducer": reference(Path(__file__).resolve().relative_to(Path.cwd())),
        "raw_evidence_and_inventory_preserved": True,
        "forward_forest_complete": True,
        "parent_bindings": len(source["new"]["native_structures"][0]["parents"]),
        "visited_marked_slots": len(slots),
        "target_glyphs": len(ids), "target_glyphs_unique_and_parent_bound_in_declared_order": True,
        "containing_chunk_glyphs": len(chunk), "target_node_checks": node_checks,
        "interior_literal_spaces_outside_frozen_target": omitted,
        "preserved_events": len(current_events), "native_region_reviews": 0,
        "remaining": "The target-bearing second-page node also contains the next paragraph. Its whole source list crosses a repeated declaration barrier. Independently certified finite cuts and page-region closure are still required; the internal literal page-edge space cannot silently disappear to fit the frozen target.",
    }, indent=2))


if __name__ == "__main__":
    main()
