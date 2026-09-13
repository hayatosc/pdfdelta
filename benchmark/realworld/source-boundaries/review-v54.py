"""Check six additional V54 ranges against original glyphs and independent masks."""

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


# Load pure historical checks without executing their capture/assembly code.
helpers = {"Counter": Counter, "sys": sys, "itertools": itertools}
helpers["helpers"] = helpers
helper_paths = []
for filename, names in (
    ("review-v31.py", {"node_sources", "check_original_cuts", "check_paint_population"}),
    ("review-v33.py", {"projected_content"}),
    ("review-v33-controls.py", {"mandatory_unmatched"}),
    ("review-v51.py", {"expanded_view", "source_change_bounds", "check_mask"}),
):
    path = BASE / filename
    helper_paths.append(ref(path))
    for statement in ast.parse(path.read_text()).body:
        if isinstance(statement, ast.FunctionDef) and statement.name in names:
            exec(compile(ast.Module(body=[statement], type_ignores=[]), str(path), "exec"), helpers)


def checked_view(review, side, original):
    """Recheck literal spaces and synthetic separators before adapting the view."""
    glyphs = {glyph["id"]: glyph for glyph in original["native"]["items"]}
    members = set(review["source_cuts"]["population"][side])
    nodes = []
    for node in original["graph"]["nodes"]:
        if node["id"] not in members:
            nodes.append(node)
            continue
        node = copy.deepcopy(node)
        view = node["content"]["view"]
        assert view["normalization"]["kind"] in ("exact", "unresolved")
        assert len(view["tokens"]) == len(view["origins"]) == len(view["source_backed"])
        for position, (token, origins, backed) in enumerate(zip(
                view["tokens"], view["origins"], view["source_backed"])):
            scalar = token["Scalar"]
            assert len(scalar) == 1
            if backed and scalar == " ":
                assert origins and all(glyphs[atom["glyph"]]["text"] == {"Mapped": " "}
                                       for atom in origins)
            if not backed:
                assert scalar in " \t\n\r\v\f" and len(origins) == 2
                before = next(index for index in range(position - 1, -1, -1)
                              if view["source_backed"][index])
                after = next(index for index in range(position + 1, len(view["tokens"]))
                             if view["source_backed"][index])
                assert origins == [view["origins"][before][-1], view["origins"][after][0]]
                # These six ranges have no discretionary source hyphen. The
                # independent mask enumerator below varies only separators.
                assert view["tokens"][before] != {"Scalar": "-"}
        # Non-space glyphs were independently checked by projected_content.
        # This temporary adapter lets the existing enumerator accept the newly
        # checked raw view; no source file or production normalization changes.
        view["normalization"] = {"kind": "exact"}
        nodes.append(node)
    adapted = {**original, "graph": {**original["graph"], "nodes": nodes}}
    result = helpers["expanded_view"](review, side, adapted)
    def space_origin(backed, origins):
        if backed:
            return "literal_glyph"
        a, b = [glyphs[atom["glyph"]] for atom in origins]
        if a["page"] != b["page"]:
            return "page_separator"
        if a["baseline"]["y"] != b["baseline"]["y"]:
            return "line_separator"
        return "reconstructed_gap" if a["direction"] == b["direction"] else "ambiguous"

    spaces = [{"position": position,
               "origin": space_origin(backed, origins),
               "sources": origins}
              for position, (scalar, origins, backed) in enumerate(zip(
                  result["tokens"], result["origins"], result["backed"]))
              if scalar in " \t\n\r\v\f"]
    assert review["spacing"][side] == spaces
    return result


def main():
    previous = {row["pair"]: row for row in read(BASE / "v51-panel-observations.json")["observations"]
                if row["repetition"] == 1}
    directories = {
        "edpb-restrictions-v1-to-final": "source-boundaries-native-order-v33/review",
        "nist-sha-1803-to-1804": "source-boundaries-sha-v9-review",
        "irs-w2-2024-to-2025": "source-boundaries-native-order-v33/irs-w2-2024-to-2025-source-review",
    }
    checks = []
    for observation in read(BASE / "v54-panel-observations.json")["observations"]:
        pair = observation["pair"]
        report = verify.read_reference(observation["report"])
        old_events = verify.events(verify.read_reference(previous[pair]["report"]))
        old_digests = {verify.event_digest(event) for event in old_events}
        added = [event for event in verify.events(report) if verify.event_digest(event) not in old_digests]
        if not added:
            continue
        directory = ROOT / "benchmark/realworld/cache" / directories[pair]
        source_path = directory / "sources.json"
        source = read(source_path)
        for event in added:
            assert event["category"] == "B"
            review = event["review"]
            views, sides = {}, {}
            for side in ("old", "new"):
                original = source[side]
                assert original["summary"]["revision"] == report[side]["revision"]
                nodes = {node["id"]: node for node in original["graph"]["nodes"]}
                glyphs = {glyph["id"]: glyph for glyph in original["native"]["items"]}
                selected = [atom["glyph"] for atom in review[side + "_sources"]]
                assert len(selected) == len(set(selected))
                raw = [glyphs[glyph] for glyph in selected]
                assert all(set(glyph["text"]) == {"Mapped"} and glyph["crop_status"] == "Inside"
                           and glyph["path_clip_status"] in ("Inside", "Unclipped")
                           and glyph["render_mode"] in ("Fill", "Stroke", "FillAndStroke") for glyph in raw)
                projection = helpers["projected_content"](
                    review, side, original, selected, event["operation"][side])
                helpers["check_original_cuts"](review, side, original["graph"], glyphs)
                order = review["source_cuts"]["population"].get("row_order")
                paint = None
                if order:
                    scope = int(event["pointer"].split("/")[3])
                    root = report["comparison"]["scopes"][scope]["result"]["matching"]["scope"][side]
                    paint = helpers["check_paint_population"](review, side, original, root)
                views[side] = checked_view(review, side, original)
                pages = {glyph["page"] for glyph in raw}
                for edge in ("entry", "exit"):
                    pages.update(nodes[review["source_cuts"][edge][side]["node"]]["pages"])
                sides[side] = {"glyphs": len(raw), "pages": sorted(pages), "projection": projection,
                               "paint_population": paint,
                               "images": [ref(directory / f"{side}-region-{page}.png") for page in sorted(pages)]}
            proof = review["comparison"].get("text_change_proof")
            if proof:
                token = proof["token"]["Scalar"]
                for side in ("old", "new"):
                    view = views[side]
                    required = sum(scalar == token and backed and not optional for scalar, backed, optional
                                   in zip(view["tokens"], view["backed"], view["optional"]))
                    assert proof[side + "_required"] == required
                    assert proof[side + "_possible"] == view["tokens"].count(token)
            mask = helpers["check_mask"](review, views) if review["comparison"]["text_mask"] else None
            checks.append({"pair": pair, "pointer": event["pointer"], "event_sha256": verify.event_digest(event),
                           "report": observation["report"], "sources": ref(source_path), "sides": sides,
                           "independent_mask": mask, "checked_multiplicity": proof,
                           "verdict": "source_checks_passed_pending_page_review"})
    assert len(checks) == 6
    (BASE / "v54-added-source-checks.json").write_text(json.dumps(
        {"version": 1, "observations": ref(BASE / "v54-panel-observations.json"),
         "helpers": helper_paths, "checks": checks}, indent=2) + "\n")
    print("Checked six source ranges, space provenance, independent masks and count witnesses.")


if __name__ == "__main__":
    main()
