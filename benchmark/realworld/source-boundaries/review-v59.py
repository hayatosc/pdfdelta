"""Check added enclosing ranges against original source populations and masks."""

import ast
from collections import Counter
import copy
import hashlib
import itertools
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
BASE = ROOT / "benchmark/realworld/source-boundaries"
sys.path.insert(0, str(ROOT / "benchmark/realworld/remaining"))
import verify

verify.CONTRACT = "source-boundaries-v1"


def read(path):
    return json.loads(path.read_text())


def ref(path):
    return {"path": str(path.relative_to(ROOT)),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


# Historical capture scripts are immutable; only their pure checks are loaded.
helpers = {"Counter": Counter, "copy": copy, "sys": sys, "itertools": itertools}
helpers["helpers"] = helpers
helper_paths = []
for filename, names in (
    ("review-v31.py", {"node_sources", "check_original_cuts"}),
    ("review-v33.py", {"projected_content"}),
    ("review-v33-controls.py", {"mandatory_unmatched"}),
    ("review-v51.py", {"expanded_view", "source_change_bounds", "check_mask"}),
    ("review-v54.py", {"checked_view"}),
):
    path = BASE / filename
    helper_paths.append(ref(path))
    for statement in ast.parse(path.read_text()).body:
        if isinstance(statement, ast.FunctionDef) and statement.name in names:
            exec(compile(ast.Module(body=[statement], type_ignores=[]), str(path), "exec"), helpers)


def check_standard_population(review, side, original, root):
    """Independently census the strict descending, single-page source band."""
    population = review["source_cuts"]["population"]
    assert population["kind"] == "matched_interval"
    assert not population.get("row_order") and not population.get("boundary_padding")
    nodes = {node["id"]: node for node in original["graph"]["nodes"]}
    glyphs = {glyph["id"]: glyph for glyph in original["native"]["items"]}
    path = [nodes[node] for node in population[side]]
    refs = [atom for node in path for atom in node["sources"]]
    assert refs and all(atom["origin"] == "native" for atom in refs)
    ids = [atom["glyph"] for atom in refs]
    assert len(ids) == len(set(ids))
    raw = [glyphs[glyph] for glyph in ids]
    pages = {glyph["page"] for glyph in raw}
    assert len(pages) == 1
    page = pages.pop()
    assert all(node["pages"] == [page] for node in path)
    assert all(set(glyph["text"]) == {"Mapped"}
               and glyph["direction"] == {"x": 1.0, "y": 0.0}
               and glyph["crop_status"] == "Inside"
               and glyph["path_clip_status"] in ("Inside", "Unclipped")
               and glyph["render_mode"] in ("Fill", "Stroke", "FillAndStroke")
               for glyph in raw)
    ranges = [(min(glyphs[a["glyph"]]["baseline"]["y"] for a in n["sources"]),
               max(glyphs[a["glyph"]]["baseline"]["y"] for a in n["sources"])) for n in path]
    assert all(a[0] > b[1] for a, b in zip(ranges, ranges[1:]))
    x0 = min(glyph["bbox"]["min"]["x"] for glyph in raw)
    x1 = max(glyph["bbox"]["max"]["x"] for glyph in raw)
    y0, y1 = min(a for a, _ in ranges), max(b for _, b in ranges)
    selected = set(ids)
    band = {glyph["id"] for glyph in glyphs.values() if glyph["page"] == page
            and y0 <= glyph["baseline"]["y"] <= y1
            and glyph["bbox"]["max"]["x"] >= x0 and glyph["bbox"]["min"]["x"] <= x1}
    assert band == selected
    members = set(population[side])
    for alternative in original["graph"]["alternatives"]:
        assert alternative["parent"] != root and alternative["parent"] not in members
        assert all(not members.intersection(partition) for partition in alternative["partitions"])
    for conflict in original["graph"]["source_conflicts"]:
        assert not selected.intersection(atom.get("glyph") for atom in conflict["sources"])
    expected = {glyph["id"] for glyph in glyphs.values() if glyph["page"] == page}
    inventories = [inv for inv in original["inventories"]
                   if inv["channel"] == "text" and inv["page"] in (None, page)]
    assert inventories
    for inventory in inventories:
        assert inventory["page"] == page
        assert original["summary"]["backends"][inventory["backend"]]["kind"] == "native_parser"
        assert len(inventory["sources"]) == len(expected)
        assert {atom["glyph"] for atom in inventory["sources"]} == expected
    assert not [issue for issue in original["summary"]["issues"]
                if issue["channel"] == "text" and issue["page"] in (None, page)]
    paints = [paint for paint in original["native"]["non_text_paint_bounds"] if paint["page"] == page]
    ink_y0 = min(glyph["bbox"]["min"]["y"] for glyph in raw)
    ink_y1 = max(glyph["bbox"]["max"]["y"] for glyph in raw)
    for paint in paints:
        box = paint["bounds"]
        assert box is not None
        assert (box["max"]["x"] < x0 or box["min"]["x"] > x1
                or box["max"]["y"] < ink_y0 or box["min"]["y"] > ink_y1)
    if not all(inv["complete"] for inv in inventories):
        assert str(page) in original["native"]["last_non_text_paint"]
    return {"profile": "strict-descending-page-band", "page": page,
            "population_glyphs": len(raw), "native_inventory_glyphs": len(expected),
            "page_text_inventory_complete": all(inv["complete"] for inv in inventories),
            "disjoint_non_text_paint": paints}


def main():
    pilot = read(BASE / "unseen-v5-diagnosis/v59-pilot.json")
    checks = []
    for row in pilot["rows"]:
        if not row["added"]:
            continue
        report = verify.read_reference(row["report"])
        source = verify.read_reference(row["source_export"])
        for event in verify.events(report):
            if verify.event_digest(event) not in row["added"]:
                continue
            assert event["category"] == "B"
            review = event["review"]
            scope = int(event["pointer"].split("/")[3])
            roots = report["comparison"]["scopes"][scope]["result"]["matching"]["scope"]
            sides, views = {}, {}
            for side in ("old", "new"):
                original = source[side]
                assert original["summary"]["revision"] == report[side]["revision"]
                glyphs = {glyph["id"]: glyph for glyph in original["native"]["items"]}
                selected = [atom["glyph"] for atom in review[side + "_sources"]]
                helpers["check_original_cuts"](review, side, original["graph"], glyphs)
                projection = helpers["projected_content"](
                    review, side, original, selected, event["operation"][side])
                census = check_standard_population(review, side, original, roots[side])
                if review["comparison"]["text_mask"]:
                    views[side] = helpers["checked_view"](review, side, original)
                sides[side] = {"glyphs": len(selected), "projection": projection, "census": census}
            proof = review["comparison"].get("text_change_proof")
            if proof:
                scalar = proof["token"]["Scalar"]
                assert scalar not in " \t\n\r\v\f-"
                for side in ("old", "new"):
                    glyphs = {glyph["id"]: glyph for glyph in source[side]["native"]["items"]}
                    count = sum(glyphs[a["glyph"]]["text"]["Mapped"].count(scalar)
                                for a in review[side + "_sources"])
                    assert proof[side + "_required"] == proof[side + "_possible"] == count
            mask = helpers["check_mask"](review, views) if views else None
            checks.append({"pair": row["pair"], "pointer": event["pointer"],
                           "event_sha256": verify.event_digest(event), "report": row["report"],
                           "sources": row["source_export"], "sides": sides,
                           "independent_mask": mask, "checked_multiplicity": proof,
                           "verdict": "source_checks_passed_pending_page_review"})
            print(row["pair"], event["pointer"], "source checks passed", flush=True)
    assert len(checks) == 3
    (BASE / "v59-added-source-checks.json").write_text(json.dumps(
        {"version": 1, "pilot": ref(BASE / "unseen-v5-diagnosis/v59-pilot.json"),
         "helpers": helper_paths, "checks": checks}, indent=2) + "\n")


if __name__ == "__main__":
    main()
