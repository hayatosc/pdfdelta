"""Independently check native control masks against literal source populations."""

from collections import defaultdict
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
BASE = ROOT / "benchmark/realworld/source-boundaries"
CONTROL = ROOT / "benchmark/realworld/next/layout-controls"
sys.path.insert(0, str(ROOT / "benchmark/realworld/remaining"))
import verify

verify.CONTRACT = "source-boundaries-v1"
verify.DIRECTORY = BASE


def read(path):
    return json.loads(path.read_text())


def ref(path):
    return {"path": str(path.relative_to(ROOT)),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def save(name, value):
    (BASE / name).write_text(json.dumps(value, indent=2) + "\n")


def mandatory_unmatched(old, new):
    """Find tokens unmatched by every optimal insertion/deletion alignment."""
    def lengths(a, b):
        result = [[0] * (len(b) + 1) for _ in range(len(a) + 1)]
        for i, left in enumerate(a):
            for j, right in enumerate(b):
                result[i + 1][j + 1] = (result[i][j] + 1 if left == right else
                                       max(result[i][j + 1], result[i + 1][j]))
        return result

    prefix, suffix = lengths(old, new), lengths(old[::-1], new[::-1])
    optimum = prefix[-1][-1]
    matched_old, matched_new = set(), set()
    for i, left in enumerate(old):
        for j, right in enumerate(new):
            if left == right and prefix[i][j] + 1 + suffix[len(old) - i - 1][len(new) - j - 1] == optimum:
                matched_old.add(i)
                matched_new.add(j)
    return set(range(len(old))) - matched_old, set(range(len(new))) - matched_new


observations = read(BASE / "v33-control-observations.json")
exports = {row["pair"]: row for row in read(BASE / "v33-control-source-observations.json")["observations"]}
page_review = read(BASE / "v33-control-page-review.json")
reviewed_images = {reference["path"]: reference for reference in page_review["viewed_images"]}
for reference in reviewed_images.values():
    verify.historical.checked_path(reference)
for row in page_review["identical_images"]:
    assert row["identical_viewed_image"] in page_review["viewed_images"]
    assert row["image"]["sha256"] == row["identical_viewed_image"]["sha256"]
    verify.historical.checked_path(row["image"])
    reviewed_images[row["image"]["path"]] = row["image"]
proofs, reviews = [], []
for observation in observations["reports"]:
    pair, route = observation["pair"], observation["route"]
    if route != "native" or not ("-affiliation-" in pair or pair in exports):
        continue
    report = verify.read_reference(observation["report"])
    events = [event for event in verify.events(report) if event["category"] == "A"]
    if not events:
        continue
    annotation = ref(CONTROL / "annotations" / f"{pair}.json")
    resolution = ref(CONTROL / "annotations" / f"{pair}.resolved.json")
    evidence = [annotation, resolution]
    populations = {}
    if pair not in exports:
        authored, resolved = verify.read_reference(annotation), verify.read_reference(resolution)
        by_id = {row["id"]: row for row in resolved["selectors"]}
        sides = {}
        for side in ("old", "new"):
            selectors = [row for row in authored["selectors"]
                         if row["side"] == side and "-paragraph-3-" in row["id"]]
            assert len(selectors) == 1
            selector = selectors[0]
            rows = verify.historical.source_rows(by_id[selector["id"]])
            assert len(rows) == len(selector["literal_quote"])
            assert all(len(row) == 1 and row[0]["kind"] == "glyph" for row in rows)
            sides[side] = {"text": selector["literal_quote"], "ids": [row[0]["id"] for row in rows]}
        assert sides["old"]["text"].replace(" ", "") == "ThesamplebelongstoOrion."
        assert sides["new"]["text"].replace(" ", "") == "ThesamplebelongstoLyra."
        populations["affiliation"] = sides
        event_populations = {event["pointer"]: "affiliation" for event in events}
    else:
        export = exports[pair]
        source = verify.read_reference(export["sources"])
        evidence += [export["sources"], export["manifest"],
                     ref(BASE / "v33-control-source-observations.json"),
                     ref(BASE / "v33-control-page-review.json")]
        glyphs = {side: {g["id"]: g for g in source[side]["native"]["items"]} for side in ("old", "new")}
        nodes = {side: [n for n in source[side]["graph"]["nodes"]
                       if n["kind"] == "paragraph" and n["basis"] == {"kind": "native_layout"}]
                 for side in ("old", "new")}

        def population(side, node):
            ids = [r["glyph"] for r in node["sources"] if r["origin"] == "native"]
            assert len(ids) == len(node["sources"]) == len(set(ids))
            items = [glyphs[side][i] for i in ids]
            assert all(len(g["text"]["Mapped"]) == 1 for g in items)
            assert all(g["crop_status"] == "Inside" and g["path_clip_status"] in ("Inside", "Unclipped")
                       and g["render_mode"] in ("Fill", "Stroke", "FillAndStroke") for g in items)
            return {"node": node["id"], "text": "".join(g["text"]["Mapped"] for g in items),
                    "ids": ids, "pages": node["pages"]}

        event_populations = {}
        for event in events:
            sides = {}
            for side in ("old", "new"):
                selected = {i for s, i in event["sources"] if s == side}
                if selected:
                    found = [node for node in nodes[side]
                             if selected <= {r.get("glyph") for r in node["sources"]}]
                else:
                    # The page-reviewed footer is the only opposite-side source
                    # population for the two partial insertion/deletion runs.
                    literal = "Schedule C (Form 1040) 2024 "
                    found = [node for node in nodes[side] if population(side, node)["text"] == literal]
                assert len(found) == 1, (pair, event["pointer"], side)
                sides[side] = population(side, found[0])
                for page in sides[side]["pages"]:
                    image = ref(ROOT / export["sources"]["path"].rsplit("/", 1)[0] / f"{side}-region-{page}.png")
                    assert reviewed_images[image["path"]] == image
                    evidence.append(image)
            key = f"{sides['old']['node']}:{sides['new']['node']}"
            populations[key] = sides
            event_populations[event["pointer"]] = key
    expected, claimed = {}, defaultdict(set)
    for key, sides in populations.items():
        masks = mandatory_unmatched(sides["old"]["text"], sides["new"]["text"])
        expected[key] = {(side, sides[side]["ids"][position])
                         for side, positions in zip(("old", "new"), masks) for position in positions}
    for event in events:
        key = event_populations[event["pointer"]]
        assert event["sources"] <= expected[key], (pair, event["pointer"], event["sources"] - expected[key])
        assert not claimed[key] & event["sources"]
        claimed[key].update(event["sources"])
        change = report["changes"][int(event["pointer"].rsplit("/", 1)[1])]
        for occurrence in change["occurrences"]:
            for side in ("old", "new"):
                span = occurrence[side + "_span"]
                if span is None:
                    continue
                characters = dict(zip(populations[key][side]["ids"], populations[key][side]["text"]))
                assert "".join(characters[r["glyph_id"]] for r in span["sources"]) == span["text"]
        proofs.append({"pair": pair, "pointer": event["pointer"], "event_sha256": verify.event_digest(event),
                       "population": populations[key], "mandatory_changed_sources": sorted(expected[key]),
                       "claimed_sources": sorted(event["sources"]), "source_evidence": evidence})
        # The first real-source numeric event already has immutable exact gold.
        if pair in exports and event["pointer"] == "/changes/0":
            continue
        reviews.append({"pair": pair, "route": route, "pointer": event["pointer"],
                        "report": observation["report"], "event_sha256": verify.event_digest(event),
                        "verdict": "source_supported", "source_evidence": evidence,
                        "source_content_rationale": (
                            "Orion changes to Lyra; the retained r is unchanged. All claimed source tokens are unmatched in every optimal literal alignment."
                            if pair not in exports else
                            "The annual form changes 2024/2025 or adds/removes its creation-date footer. Every claimed source token is unmatched in all optimal alignments of the corresponding original passage."),
                        "correspondence_rationale": (
                            "The preregistered literal selectors identify the same authored paragraph under the presentation mutation."
                            if pair not in exports else
                            "The source and page review identify the corresponding question or footer; object renaming, the blank-page shift and reversal retain those same original glyph populations.")})
    assert dict(claimed) == expected, pair
save("v33-control-source-checks.json", {"version": 1, "proofs": proofs})
proof_reference = ref(BASE / "v33-control-source-checks.json")
for review in reviews:
    review["source_evidence"] += [proof_reference, ref(Path(__file__).resolve())]
    review["source_evidence"] = list({(r["path"], r["sha256"]): r for r in review["source_evidence"]}.values())
observations["adjudications"] = reviews
save("v33-reviewed-control-observations.json", observations)
result = verify.controls(observations, read(ROOT / "benchmark/realworld/followup/controls.json"),
                         ref(ROOT / "benchmark/realworld/cache/source-boundaries-native-order-v33/pdfdelta"))
save("v33-reviewed-control-results.json", result)
print(f"Checked {len(proofs)} native masks; {len(reviews)} additional source reviews; controls correct: {result['correct']}")
