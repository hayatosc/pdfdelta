"""Revalidate selected frozen outputs against retained source and page reviews."""

from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import shutil
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benchmark/realworld/remaining"))
import verify

verify.CONTRACT = "source-boundaries-v1"
BASE = ROOT / "benchmark/realworld/source-boundaries"


def read(path):
    return json.loads(path.read_text())


def ref(path):
    return {"path": str(path.relative_to(ROOT)), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def save(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def node_sources(node, start, end):
    view = node["content"]["view"]
    assert 0 <= start <= end <= len(view["tokens"])
    selected = {source["glyph"] for position in range(start, end)
                if view["source_backed"][position]
                for source in view["origins"][position] if source["origin"] == "native"}
    return [source for source in node["sources"]
            if source["origin"] == "native" and source["glyph"] in selected]


def check_original_cuts(review, side, graph, glyphs):
    cuts = review.get("source_cuts")
    if cuts is None:
        return
    nodes = {node["id"]: node for node in graph["nodes"]}
    population = cuts["population"]
    assert population["kind"] == "matched_interval"
    order = population[side]
    first, last = cuts["entry"][side], cuts["exit"][side]
    a, b = order.index(first["node"]), order.index(last["node"])
    expected = []
    for index in range(a, b + 1):
        node = nodes[order[index]]
        start = first["token_boundary"] if index == a else 0
        end = last["token_boundary"] if index == b else len(node["content"]["view"]["tokens"])
        expected.extend(node_sources(node, start, end))
    assert expected == review[side + "_sources"]
    refinement = cuts.get("edge_refinement")
    if refinement is not None:
        for edge in refinement[side + "_padding"]:
            for fragment in edge:
                assert node_sources(nodes[fragment["node"]], *fragment["tokens"]) == fragment["sources"]
                assert all(glyphs[source["glyph"]]["text"] == {"Mapped": " "}
                           for source in fragment["sources"])


EDPB = "edpb-controller-processor-v1-to-v2-1"
DESIGN = "edpb-design-default-v1-to-v2"
INCIDENT = "nist-incident-handling-r2-to-r3"
SHA = "nist-sha-1803-to-1804"
BERT = "arxiv-bert-v1-to-v2"
selected = {EDPB, DESIGN, INCIDENT, SHA, BERT}
build = read(ROOT / "benchmark/realworld/cache/source-boundaries-local-cuts-v19/build.json")
assert build["production_sha256"] == verify.source_fingerprint()
evaluator = ROOT / "benchmark/realworld/cache/source-boundaries-local-cuts-v19-evaluator"
evaluator.mkdir(exist_ok=True)
for name in ("verify.py", "test_verify.py"):
    shutil.copy2(ROOT / "benchmark/realworld/remaining" / name, evaluator / name)

prior_index = read(BASE / "v12-adjudications.json")
prior = {(row["pair"], event["event_sha256"]): event
         for row in prior_index["observations"] for event in row["events"]}
observations = [row for row in read(BASE / "v19-pilot-observations.json")["observations"]
                if row["pair"] in selected]
observations.extend(read(BASE / "v19-reviewed-repetition2.json")["observations"])
targets = {target["pair"]: target for target in read(ROOT / "benchmark/realworld/followup/targets.json")["targets"]}
baselines = {"historical": read(ROOT / "benchmark/realworld/followup/baseline-observations.json"),
             "599855d": read(BASE / "baseline-observations.json")}
directories = {EDPB: "source-boundaries-baseline-edpb-review", DESIGN: "source-boundaries-design-v12-review",
               INCIDENT: "source-boundaries-incident-v15-review", SHA: "source-boundaries-sha-v9-review"}
sources_cache = {}

# These rationales reflect page-image and native-source review. They are not
# inferred from the diff engine's own declaration that a range changed.
reasons = {
    (EDPB, 9): "The first line of paragraph 73 becomes paragraph 75. The visible prose is unchanged at this line; numbering proves a local difference independently of reconstructed versus literal word spaces. This is not another prose-target recovery.",
    (DESIGN, 0): "The corresponding time-of-determination heading gains label 2.1.4.1, paragraph 32 becomes 33, and must becomes shall in its first line. Both page images show these changes above the old watermark. This finite range is outside the fixed paragraph-4 target.",
    (INCIDENT, 0): "The old abstract describes establishing incident-response capabilities and incident-handling guidelines. The new abstract describes incorporating incident response into risk management under CSF 2.0. The full corresponding abstracts visibly differ. This original whole-node range retains three outer space glyphs and exceeds the fixed target.",
    (INCIDENT, 1): "This independently source-projected parent retains the same 1391-atom abstract range as the original whole-node review, including two old and one new outer space glyphs. It supports the finer cut certificate and is not a second abstract change or a fixed-extent recovery.",
    (INCIDENT, 2): "The additional abstract view contains exactly the frozen 735 old and 653 new glyphs. The two old and one new outer literal spaces remain in the parent and are recorded in the padding fragments. No interior text, heading or keyword glyph is removed to obtain the 1388-atom extent.",
    (SHA, 20): "The corresponding cover changes Carlos M. Gutierrez to Penny Pritzker as Secretary. This raw parent retains one trailing literal space per side. The same source paragraph was reviewed in v12.",
    (SHA, 21): "The Secretary name change is retained in an additional content view without the two outer literal spaces. The cover and source glyphs agree; this is not an additional natural target.",
    (SHA, 22): "The padding paragraph removes the before-computation requirement and permits padding before computation or before the affected message blocks are processed. The raw parent retains its outer literal spaces. The paragraph's source and page images were reviewed in v12.",
    (SHA, 23): "The additional padding-paragraph view excludes four outer space glyphs while retaining the same source-supported prose change and all interior spaces. Its raw parent remains reported.",
    (SHA, 24): "The security paragraph adds SHA-512/224 and SHA-512/256. The source still says five algorithms; no correction to the author's wording is inferred. This raw parent retains four trailing literal spaces in total.",
    (SHA, 25): "The additional security-paragraph view retains the added algorithm names and excludes only the four outer literal spaces recorded in its parent certificate.",
    (SHA, 26): "The corresponding SP 800-57 reference tail changes August 2005 to (Draft) May 2011. This raw parent retains two trailing literal spaces per side. The reference was source-reviewed in v12.",
    (SHA, 27): "The additional reference-tail view retains the date and draft-qualifier change while excluding only the four outer literal spaces. It is not a new prose-target recovery.",
}
reasons[DESIGN, 1] = reasons[DESIGN, 0] + " The additional raw parent retains the original range and its uncertain layout spaces."
reasons[DESIGN, 2] = reasons[DESIGN, 0] + " The finer view excludes only one old mandatory outer space; uncertain new layout spaces remain quantified rather than declared literal."

adjudications, scores = [], []
for observation in observations:
    pair = observation["pair"]
    report_path = ROOT / observation["report"]["path"]
    assert ref(report_path) == observation["report"]
    assert ref(ROOT / observation["capture"]["path"]) == observation["capture"]
    report = read(report_path)
    events = verify.events(report)
    rows = []
    for event in events:
        digest = verify.event_digest(event)
        if (pair, digest) in prior:
            record = copy.deepcopy(prior[pair, digest])
            record["pointer"] = event["pointer"]
            record["identity_revalidation"] = "The frozen v19 event/source digest equals the source-reviewed v12 digest."
        else:
            index = int(event["pointer"].rsplit("/", 1)[1])
            rationale = reasons[pair, index]
            review = event["review"]
            source_path = ROOT / "benchmark/realworld/cache" / directories[pair] / "sources.json"
            if pair not in sources_cache:
                sources_cache[pair] = read(source_path)
            sources = sources_cache[pair]
            evidence = {"sources": ref(source_path), "sides": {}}
            for side in ("old", "new"):
                assert report[side]["revision"] == sources[side]["summary"]["revision"]
                glyphs = {glyph["id"]: glyph for glyph in sources[side]["native"]["items"]}
                glyphs_in_range = [glyphs[source["glyph"]] for source in review[side + "_sources"]]
                raw = "".join(glyph["text"]["Mapped"] for glyph in glyphs_in_range)
                displayed = review["comparison"]["operation"][side]
                assert raw.replace(" ", "") == displayed.replace(" ", "")
                assert all(glyph["crop_status"] == "Inside"
                           and glyph["path_clip_status"] in ("Inside", "Unclipped")
                           and glyph["render_mode"] in ("Fill", "Stroke", "FillAndStroke")
                           for glyph in glyphs_in_range)
                check_original_cuts(review, side, sources[side]["graph"], glyphs)
                pages = sorted({glyph["page"] for glyph in glyphs_in_range})
                evidence["sides"][side] = {"pages": pages, "raw_text": raw, "displayed_text": displayed,
                    "raw_source_spaces": raw.count(" "), "exact_raw_text": raw == displayed,
                    "images": [ref(source_path.parent / f"{side}-region-{page}.png") for page in pages]}
            record = {"pointer": event["pointer"], "event_sha256": digest, "verdict": "source_supported",
                "source_content_rationale": rationale,
                "correspondence_rationale": "Accepted, mandatory, non-inferred boundaries delimit the corresponding finite source interval. The acquired source and page views support that conditional correspondence. Source cuts retain the independently closed population; literal-space edge fragments bind to their retained raw parent. Raw source gaps are not promoted to exact literal spaces or exact masks.",
                "source_evidence": evidence}
        rows.append(record)
    adjudications.append({**observation, "events": rows})
    core, extent, controls = verify.historical.target_sources(targets[pair])
    for name, baseline_index in baselines.items():
        previous = next(row for row in baseline_index["observations"]
                        if row["pair"] == pair and row["repetition"] == observation["repetition"])
        unavailable = None
        if not (ROOT / previous["report"]["path"]).exists():
            unavailable = previous["report"]
            previous = next(row for row in baseline_index["observations"]
                            if row["pair"] == pair and (ROOT / row["report"]["path"]).exists())
        assert ref(ROOT / previous["report"]["path"]) == previous["report"]
        before = read(ROOT / previous["report"]["path"])
        counts = Counter(verify.event_digest(event) for event in verify.events(before))
        needed = []
        for event, adjudication in zip(events, rows):
            digest = verify.event_digest(event)
            if counts[digest]:
                counts[digest] -= 1
            else:
                needed.append(adjudication)
        score = verify.pair_recovery(before, report, core, extent, controls, needed)
        expected = [] if pair == DESIGN or (pair == EDPB and name == "599855d") else ["B"]
        assert score["correct"] and score["additional_categories"] == expected, (pair, name, score)
        scores.append({"pair": pair, "repetition": observation["repetition"], "baseline": name,
            "baseline_repetition": previous["repetition"], "unavailable_registered_report": unavailable,
            "baseline_report": previous["report"], "target_atoms": len(extent), **score})

save(BASE / "v19-adjudications.json", {"version": 1,
    "scope": "All B outputs for four selected target pairs in two observations, plus the design-default miss in one observation. Raw parents and refined views are not distinct author edits or natural pairs.",
    "inherited_adjudications": ref(BASE / "v12-adjudications.json"), "observations": adjudications})
save(BASE / "v19-target-scores.json", {"version": 1, "evaluator": ref(evaluator / "verify.py"),
    "adjudications": ref(BASE / "v19-adjudications.json"),
    "target_references": {pair: targets[pair]["references"] for pair in sorted(selected)}, "scores": scores,
    "status": "Four reviewed natural pairs from three producers score B against the surviving historical baseline. EDPB is retained 599855d capability; BERT, NIST SHA and NIST incident handling are additional relative to that baseline. The design-default target remains missed. The six-pair development, natural segmented recovery and new unseen gates remain unmet."})
print(json.dumps(scores, indent=2))
