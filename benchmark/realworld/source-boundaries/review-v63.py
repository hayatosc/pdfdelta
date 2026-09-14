"""Adjudicate the additional MobileNetV2 interval from frozen V63 captures."""

from collections import Counter
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
BASE = ROOT / "benchmark/realworld/source-boundaries"
sys.path.insert(0, str(ROOT / "benchmark/realworld/remaining"))
import verify

verify.CONTRACT = "source-boundaries-v1"
PAIR = "arxiv-mobilenetv2-v1-to-v4"


def read(path):
    return json.loads(path.read_text())


def ref(path):
    return {"path": str(path.relative_to(ROOT)),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def check_band(review, side, source):
    """Census the original glyphs between the retained whole-node endpoints."""
    glyphs = {g["id"]: g for g in source["native"]["items"]}
    body = review[side + "_sources"]
    first, last = review[side + "_boundaries"]
    population = first + body + last
    assert all(r["origin"] == "native" for r in population)
    ids = [r["glyph"] for r in population]
    assert len(ids) == len(set(ids))
    raw = [glyphs[i] for i in ids]
    pages = {g["page"] for g in raw}
    assert len(pages) == 1
    page = pages.pop()
    assert all(set(g["text"]) == {"Mapped"}
               and g["direction"] == {"x": 1.0, "y": 0.0}
               and g["crop_status"] == "Inside"
               and g["path_clip_status"] in ("Inside", "Unclipped")
               and g["render_mode"] in ("Fill", "Stroke", "FillAndStroke") for g in raw)
    assert all(a["render_order"] < b["render_order"] for a, b in zip(raw, raw[1:]))
    x0 = min(g["bbox"]["min"]["x"] for g in raw)
    x1 = max(g["bbox"]["max"]["x"] for g in raw)
    y0 = min(g["baseline"]["y"] for g in raw)
    y1 = max(g["baseline"]["y"] for g in raw)
    census = {g["id"] for g in glyphs.values() if g["page"] == page
              and y0 <= g["baseline"]["y"] <= y1
              and g["bbox"]["max"]["x"] >= x0 and g["bbox"]["min"]["x"] <= x1}
    assert census == set(ids)
    nodes = [n for n in source["graph"]["nodes"] if n["sources"]
             and any(r.get("glyph") in census for r in n["sources"])]
    nodes.sort(key=lambda n: min(glyphs[r["glyph"]]["render_order"] for r in n["sources"]))
    assert [r for n in nodes for r in n["sources"]] == population
    ranges = [(min(glyphs[r["glyph"]]["baseline"]["y"] for r in n["sources"]),
               max(glyphs[r["glyph"]]["baseline"]["y"] for r in n["sources"])) for n in nodes]
    assert all(a[0] > b[1] for a, b in zip(ranges, ranges[1:]))
    members = {n["id"] for n in nodes}
    for alternative in source["graph"]["alternatives"]:
        assert alternative["parent"] not in members | {0}
        assert all(not set(partition) & members for partition in alternative["partitions"])
    for conflict in source["graph"]["source_conflicts"]:
        assert not {r.get("glyph") for r in conflict["sources"]} & census
    inventories = [i for i in source["inventories"]
                   if i["channel"] == "text" and i["page"] in (None, page)]
    expected = {g["id"] for g in glyphs.values() if g["page"] == page}
    assert inventories
    for inventory in inventories:
        assert inventory["page"] == page and inventory["complete"]
        assert source["summary"]["backends"][inventory["backend"]]["kind"] == "native_parser"
        assert len(inventory["sources"]) == len(expected)
        assert {r["glyph"] for r in inventory["sources"]} == expected
    assert not [i for i in source["summary"]["issues"]
                if i["channel"] == "text" and i["page"] in (None, page)]
    assert not [p for p in source["native"]["non_text_paint_bounds"] if p["page"] == page]
    assert str(page) not in source["native"]["last_non_text_paint"]
    text = lambda refs: "".join(glyphs[r["glyph"]]["text"]["Mapped"] for r in refs)
    return {"page": page, "glyphs": len(body), "population_glyphs": len(ids),
            "nodes": [n["id"] for n in nodes], "endpoint_text": [text(first), text(last)],
            "body_text": text(body), "source_census_exact": True,
            "source_order_checked": True, "page_text_inventory_complete": True}


def main():
    cache = ROOT / "benchmark/realworld/cache/source-boundaries-column-edges-v63-rebuilt"
    original = ROOT / f"benchmark/realworld/cache/source-boundaries-unseen-v6/diagnosis-v60/{PAIR}"
    source_path = original / "source-review/sources.json"
    source = read(source_path)
    prior = {verify.event_digest(e): e for e in verify.events(read(original / "report.json"))}
    targets = BASE / "unseen-v6/targets.json"
    target = next(t for t in read(targets)["targets"] if t["pair"] == PAIR)
    core, extent, controls = verify.historical.target_sources(target)
    observations = []
    previous = None
    for repetition in (1, 2):
        directory = cache / f"mobilenet-repetition{repetition}"
        report_path = directory / f"{PAIR}-text.json"
        report = read(report_path)
        capture = read(directory / "runs.json")
        assert capture["binary_sha256"] == read(cache / "build.json")["binary"]["sha256"]
        assert len(capture["runs"]) == 1 and capture["runs"][0]["status"] == "captured"
        events = verify.events(report)
        current = {verify.event_digest(e): e for e in events}
        assert prior.keys() <= current.keys()
        assert all(prior[k]["review"] == current[k]["review"] for k in prior)
        added = [e for k, e in current.items() if k not in prior]
        assert len(added) == 1 and added[0]["category"] == "B"
        event = added[0]
        assert core <= event["sources"] <= extent and len(event["sources"]) == 1668
        assert not set().union(*(e["sources"] & controls for e in events if e["category"] == "A"))
        review = event["review"]
        assert review["convention"] == "closed-native-baseline-interval-v1"
        scope = report["comparison"]["scopes"][0]["result"]
        assert set(review["boundaries"]) <= set(scope["accepted_correspondences"])
        checks = {side: check_band(review, side, source[side]) for side in ("old", "new")}
        assert checks["old"]["endpoint_text"] == checks["new"]["endpoint_text"]
        for side in ("old", "new"):
            assert report[side]["revision"] == source[side]["summary"]["revision"]
        proof = review["comparison"]["text_change_proof"]
        assert proof["token"] == {"Scalar": ","}
        for side in ("old", "new"):
            count = Counter(checks[side]["body_text"])[","]
            assert proof[side + "_required"] == proof[side + "_possible"] == count
        assert proof["old_required"] == 7 and proof["new_required"] == 8
        assert review["comparison"]["text_mask"] is None
        signature = [(verify.event_digest(e), e["review"]) for e in events]
        assert previous is None or signature == previous
        previous = signature
        observations.append({"repetition": repetition, "report": ref(report_path),
                             "capture": ref(directory / "runs.json"), "pointer": event["pointer"],
                             "retained_prior_reviews": len(prior), "source_checks": checks})
    result = {"version": 63, "pair": PAIR, "build": ref(cache / "build.json"),
              "targets": ref(targets), "original_sources": ref(source_path),
              "reproducer": ref(Path(__file__)), "observations": observations,
              "scope": "One additional exposed-development B interval, with exact original glyph extent and a source-backed comma-multiplicity proof. Its exact edit mask remains unresolved. The fourteen retained outside-target outputs are not newly adjudicated by this check. This does not count as new unseen recovery or natural cross-region transition recovery."}
    (BASE / "unseen-v6-diagnosis/v63-mobilenet-review.json").write_text(json.dumps(result, indent=2) + "\n")
    print("Two captures: one additional 1668-atom B interval; all fourteen prior reviews retained.")


if __name__ == "__main__":
    main()
