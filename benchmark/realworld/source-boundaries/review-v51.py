"""Check V51 refinements against reviewed parents and independent source masks."""

import ast
from collections import Counter
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


# Historical scripts are immutable. Load their pure validators without running
# the old capture assembly or rewriting its evidence.
helpers = {"Counter": Counter, "sys": sys}
helper_paths = []
for name, functions in (
    ("review-v31.py", {"node_sources", "check_original_cuts"}),
    ("review-v33.py", {"projected_content"}),
    ("review-v33-controls.py", {"mandatory_unmatched"}),
):
    path = BASE / name
    helper_paths.append(ref(path))
    for statement in ast.parse(path.read_text()).body:
        if isinstance(statement, ast.FunctionDef) and statement.name in functions:
            exec(compile(ast.Module(body=[statement], type_ignores=[]), str(path), "exec"), helpers)


def expanded_view(review, side, original):
    """Retain original token coordinates, glyph groups and optional separators."""
    nodes = {node["id"]: node for node in original["graph"]["nodes"]}
    glyphs = {glyph["id"]: glyph for glyph in original["native"]["items"]}
    cuts = review["source_cuts"]
    members = cuts["population"][side]
    first, last = cuts["entry"][side], cuts["exit"][side]
    a, b = members.index(first["node"]), members.index(last["node"])
    tokens, origins, backed, optional = [], [], [], []
    for index in range(a, b + 1):
        node = nodes[members[index]]
        view = node["content"]["view"]
        assert view["normalization"] == {"kind": "exact"}
        start = first["token_boundary"] if index == a else 0
        end = last["token_boundary"] if index == b else len(view["tokens"])
        for position in range(start, end):
            token = view["tokens"][position]
            assert set(token) == {"Scalar"} and len(token["Scalar"]) == 1
            scalar = token["Scalar"]
            refs = view["origins"][position]
            source = view["source_backed"][position]
            if scalar == " " and source and len(refs) > 1:
                assert cuts["projection"] == "retained-glyph-whitespace-expansion-v1"
                assert all(glyphs[atom["glyph"]]["text"] == {"Mapped": " "} for atom in refs)
                for atom in refs:
                    tokens.append(" ")
                    origins.append([atom])
                    backed.append(True)
                    optional.append(False)
            else:
                tokens.append(scalar)
                origins.append(refs)
                backed.append(source)
                optional.append(not source and scalar in " \t\n\r\v\f")
    assert "".join(tokens) == review["comparison"]["operation"][side]
    assert all(not choice or not source for choice, source in zip(optional, backed))
    return {"tokens": tokens, "origins": origins, "backed": backed, "optional": optional}


def source_change_bounds(old, new, old_backed, new_backed):
    """Count source costs across every maximum-LCS path using rolling rows."""
    previous = [(0, 0, 0)] * (len(new) + 1)
    for i, left in enumerate(old):
        current = [(0, 0, 0)]
        for j, right in enumerate(new):
            choices = [previous[j + 1], current[j]]
            if left == right:
                length, low, high = previous[j]
                weight = int(old_backed[i]) + int(new_backed[j])
                choices.append((length + 1, low + weight, high + weight))
            optimum = max(choice[0] for choice in choices)
            choices = [choice for choice in choices if choice[0] == optimum]
            current.append((optimum, min(choice[1] for choice in choices),
                            max(choice[2] for choice in choices)))
        previous = current
    _, low, high = previous[-1]
    source_count = sum(old_backed) + sum(new_backed)
    return source_count - high, source_count - low


def check_mask(review, views):
    mask = review["comparison"]["text_mask"]
    choices = [(side, position) for side in ("old", "new")
               for position, optional in enumerate(views[side]["optional"]) if optional]
    assert len(choices) <= 8
    mandatory = {side: {index for index, source in enumerate(views[side]["backed"]) if source}
                 for side in ("old", "new")}
    bounds = []
    for keep in itertools.product((False, True), repeat=len(choices)):
        removed = {choice for choice, retained in zip(choices, keep) if not retained}
        positions = {side: [i for i in range(len(views[side]["tokens"])) if (side, i) not in removed]
                     for side in ("old", "new")}
        texts = {side: "".join(views[side]["tokens"][i] for i in positions[side])
                 for side in ("old", "new")}
        backed = {side: [views[side]["backed"][i] for i in positions[side]]
                  for side in ("old", "new")}
        masks = helpers["mandatory_unmatched"](texts["old"], texts["new"])
        for side, indices in zip(("old", "new"), masks):
            mandatory[side] &= {positions[side][i] for i in indices}
        bounds.append(source_change_bounds(texts["old"], texts["new"], backed["old"], backed["new"]))
    claims = mask["claims"]
    assert claims["normalization_pairs"] == len(bounds)
    assert claims["changed_source_lower"] == min(low for low, _ in bounds)
    assert claims["changed_source_upper"] == max(high for _, high in bounds)
    for side in ("old", "new"):
        assert claims["mandatory_" + side] == [i in mandatory[side] for i in range(len(views[side]["tokens"]))]
        assert mask[side] == [{"position": i, "sources": views[side]["origins"][i]}
                              for i in sorted(mandatory[side])]
    return {"normalization_pairs": len(bounds), "source_lower": claims["changed_source_lower"],
            "source_upper": claims["changed_source_upper"],
            "mandatory_positions": {side: sorted(indices) for side, indices in mandatory.items()}}


def main():
    prior_index = read(BASE / "v40-panel-observations.json")
    prior_rows = {row["pair"]: row for row in prior_index["observations"]}
    prior_reviews = {(row["pair"], event["event_sha256"]): event
                     for row in read(BASE / "v40-adjudications.json")["observations"]
                     for event in row["events"]}
    observations = read(BASE / "v51-panel-observations.json")
    directories = {
        "edpb-restrictions-v1-to-final": "source-boundaries-native-order-v33/review",
        "nist-sha-1803-to-1804": "source-boundaries-sha-v9-review",
    }
    records = []
    for observation in observations["observations"]:
        pair = observation["pair"]
        report = verify.read_reference(observation["report"])
        previous = verify.read_reference(prior_rows[pair]["report"])
        old_events = verify.events(previous)
        old_digests = {verify.event_digest(event) for event in old_events}
        for event in verify.events(report):
            digest = verify.event_digest(event)
            if digest in old_digests:
                continue
            assert pair in directories and event["category"] == "B"
            directory = ROOT / "benchmark/realworld/cache" / directories[pair]
            source_path = directory / "sources.json"
            source = read(source_path)
            review = event["review"]
            population = review["source_cuts"]["population"]
            assert population["kind"] == "matched_interval"
            assert not population.get("row_order") and not population.get("boundary_padding")
            parents = [old for old in old_events if old["review"].get("source_cuts") is None
                       and old["review"]["boundaries"] == population["boundaries"]]
            assert len(parents) == 1
            parent = parents[0]
            inherited = prior_reviews[pair, verify.event_digest(parent)]
            assert inherited["verdict"] == "source_supported" and event["sources"] <= parent["sources"]
            views, sides = {}, {}
            for side in ("old", "new"):
                original = source[side]
                assert original["summary"]["revision"] == report[side]["revision"] == previous[side]["revision"]
                nodes = {node["id"]: node for node in original["graph"]["nodes"]}
                glyphs = {glyph["id"]: glyph for glyph in original["native"]["items"]}
                assert population[side][1:-1] == parent["review"]["comparison"][side]
                refs = [atom for node in population[side][1:-1] for atom in nodes[node]["sources"]]
                assert refs == parent["review"][side + "_sources"]
                helpers["check_original_cuts"](review, side, original["graph"], glyphs)
                selected = [atom["glyph"] for atom in review[side + "_sources"]]
                raw = [glyphs[glyph] for glyph in selected]
                assert len(selected) == len(set(selected))
                assert all(set(glyph["text"]) == {"Mapped"} and glyph["crop_status"] == "Inside"
                           and glyph["path_clip_status"] in ("Inside", "Unclipped")
                           and glyph["render_mode"] in ("Fill", "Stroke", "FillAndStroke") for glyph in raw)
                projection = helpers["projected_content"](review, side, original, selected, event["operation"][side])
                views[side] = expanded_view(review, side, original)
                pages = sorted({glyph["page"] for glyph in raw})
                sides[side] = {"pages": pages, "glyphs": len(raw), "projection": projection,
                               "images": [ref(directory / f"{side}-region-{page}.png") for page in pages]}
            proof = review["comparison"].get("text_change_proof")
            if proof:
                token = proof["token"]["Scalar"]
                for side in ("old", "new"):
                    view = views[side]
                    required = sum(scalar == token and backed and not optional
                                   for scalar, backed, optional in zip(view["tokens"], view["backed"], view["optional"]))
                    possible = view["tokens"].count(token)
                    assert proof[side + "_required"] == required
                    assert proof[side + "_possible"] == possible
            mask = check_mask(review, views) if review["comparison"]["text_mask"] else None
            records.append({"pair": pair, "pointer": event["pointer"], "event_sha256": digest,
                            "report": observation["report"], "sources": ref(source_path),
                            "reviewed_parent_digest": verify.event_digest(parent),
                            "reviewed_parent": ref(BASE / "v40-adjudications.json"),
                            "source_content_rationale": inherited["source_content_rationale"],
                            "sides": sides, "independent_mask": mask, "checked_multiplicity": proof,
                            "verdict": "source_checks_passed_pending_page_review"})
    assert len(records) == 16
    (BASE / "v51-added-source-checks.json").write_text(json.dumps(
        {"version": 1, "observations": ref(BASE / "v51-panel-observations.json"),
         "helpers": helper_paths, "checks": records}, indent=2) + "\n")
    print("Checked 16 source refinements and their independent masks/count witnesses.")


if __name__ == "__main__":
    main()
