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


def check_paint_population(review, side, source, root):
    """Check new paint populations directly against the independent raw export."""
    population = review["source_cuts"]["population"]
    order = population.get("row_order")
    assert order and order["convention"] == "horizontal-paint-row-boundaries-v1"
    nodes = {node["id"]: node for node in source["graph"]["nodes"]}
    glyphs = {glyph["id"]: glyph for glyph in source["native"]["items"]}
    path = [nodes[node] for node in population[side]]
    refs = [ref for node in path for ref in node["sources"]]
    assert all(ref["origin"] == "native" for ref in refs)
    ids = [ref["glyph"] for ref in refs]
    selected_glyphs = set(ids)
    assert len(ids) == len(selected_glyphs)
    claimed = [glyphs[glyph] for glyph in ids]
    pages = {glyph["page"] for glyph in claimed}
    assert len(pages) == 1
    page = pages.pop()
    assert all(node["pages"] == [page] for node in path)
    safe = lambda glyph: (set(glyph["text"]) == {"Mapped"}
        and glyph["direction"] == {"x": 1.0, "y": 0.0}
        and glyph["crop_status"] == "Inside"
        and glyph["path_clip_status"] in ("Inside", "Unclipped")
        and glyph["render_mode"] in ("Fill", "Stroke", "FillAndStroke"))
    assert all(safe(glyph) for glyph in claimed)
    same_row = lambda a, b: abs(a - b) <= 8 * sys.float_info.epsilon * max(abs(a), abs(b))
    assert all(a["render_order"] < b["render_order"] for a, b in zip(claimed, claimed[1:]))
    rows = []
    for glyph in claimed:
        if not rows or not same_row(rows[-1][0]["baseline"]["y"], glyph["baseline"]["y"]):
            if rows:
                assert glyph["baseline"]["y"] < rows[-1][0]["baseline"]["y"]
            rows.append([])
        rows[-1].append(glyph)
    for row in rows:
        pieces = []
        for index, glyph in enumerate(row):
            if index == 0 or glyph["baseline"]["x"] < row[index - 1]["baseline"]["x"]:
                pieces.append([])
            pieces[-1].append(glyph)
        bounds = sorted((min(g["bbox"]["min"]["x"] for g in piece),
                         max(g["bbox"]["max"]["x"] for g in piece)) for piece in pieces)
        assert all(a[1] < b[0] for a, b in zip(bounds, bounds[1:]))
    if order[side] is not None:
        assert [endpoint["sources"] for endpoint in order[side]] == [path[0]["sources"], path[-1]["sources"]]
        endpoint_ids = {ref["glyph"] for endpoint in order[side] for ref in endpoint["sources"]}
        assert not endpoint_ids & {ref["glyph"] for ref in review[side + "_sources"]}
        for node in (path[0], path[-1]):
            ys = [glyphs[ref["glyph"]]["baseline"]["y"] for ref in node["sources"]]
            assert same_row(min(ys), max(ys))
    x0 = min(g["bbox"]["min"]["x"] for g in claimed)
    x1 = max(g["bbox"]["max"]["x"] for g in claimed)
    y0 = min(g["baseline"]["y"] for g in claimed)
    y1 = max(g["baseline"]["y"] for g in claimed)
    omitted = []
    for glyph in glyphs.values():
        if glyph["page"] != page or glyph["id"] in selected_glyphs:
            continue
        y = glyph["baseline"]["y"]
        if ((y < y0 and not same_row(y, y0)) or (y > y1 and not same_row(y, y1))
                or glyph["bbox"]["max"]["x"] < x0 or glyph["bbox"]["min"]["x"] > x1):
            continue
        outward = ((same_row(y, y1) and glyph["render_order"] < claimed[0]["render_order"])
                   or (same_row(y, y0) and glyph["render_order"] > claimed[-1]["render_order"]))
        row = rows[0] if same_row(y, y1) else rows[-1]
        left = min(g["bbox"]["min"]["x"] for g in row)
        right = max(g["bbox"]["max"]["x"] for g in row)
        assert outward and safe(glyph)
        assert glyph["bbox"]["max"]["x"] < left or glyph["bbox"]["min"]["x"] > right
        omitted.append(glyph["id"])
    selected = set(population[side])
    for alternative in source["graph"]["alternatives"]:
        assert alternative["parent"] != root and alternative["parent"] not in selected
        assert all(not set(partition) & selected for partition in alternative["partitions"])
    for conflict in source["graph"]["source_conflicts"]:
        assert not {ref.get("glyph") for ref in conflict["sources"]} & selected_glyphs
    expected = {glyph["id"] for glyph in glyphs.values() if glyph["page"] == page}
    inventories = [inv for inv in source["inventories"] if inv["channel"] == "text" and inv["page"] in (None, page)]
    assert inventories
    for inventory in inventories:
        assert inventory["page"] == page
        assert source["summary"]["backends"][inventory["backend"]]["kind"] == "native_parser"
        assert len(inventory["sources"]) == len(expected)
        assert {ref["glyph"] for ref in inventory["sources"]} == expected
    # These reviewed paint populations have no missing text invocation. Incomplete
    # page inventories arise from the explicitly bounded margin paint below.
    assert not [issue for issue in source["summary"]["issues"]
                if issue["channel"] == "text" and issue["page"] in (None, page)]
    ink_y0 = min(g["bbox"]["min"]["y"] for g in claimed)
    ink_y1 = max(g["bbox"]["max"]["y"] for g in claimed)
    paints = [paint for paint in source["native"]["non_text_paint_bounds"] if paint["page"] == page]
    for paint in paints:
        box = paint["bounds"]
        assert box is not None
        assert (box["max"]["x"] < x0 or box["min"]["x"] > x1
                or box["max"]["y"] < ink_y0 or box["min"]["y"] > ink_y1)
    if not all(inv["complete"] for inv in inventories):
        assert str(page) in source["native"]["last_non_text_paint"]
    return {"page": page, "population_glyphs": len(ids), "physical_rows": len(rows),
            "outside_boundary_row_glyphs": omitted,
            "native_inventory_glyphs": len(expected),
            "page_text_inventory_complete": all(inv["complete"] for inv in inventories),
            "disjoint_non_text_paint": paints}


EDPB = "edpb-controller-processor-v1-to-v2-1"
DESIGN = "edpb-design-default-v1-to-v2"
INCIDENT = "nist-incident-handling-r2-to-r3"
SHA = "nist-sha-1803-to-1804"
BERT = "arxiv-bert-v1-to-v2"
SSDF = "nist-ssdf-draft-to-final"
AUTH = "nist-authentication-63b-to-63b4"
selected = {EDPB, DESIGN, INCIDENT, SHA, BERT, SSDF, AUTH}
build = read(ROOT / "benchmark/realworld/cache/source-boundaries-local-cuts-v31/build.json")
assert build["production_sha256"] == verify.source_fingerprint()
evaluator = ROOT / "benchmark/realworld/cache/source-boundaries-local-cuts-v31-evaluator"
evaluator.mkdir(exist_ok=True)
for name in ("verify.py", "test_verify.py"):
    shutil.copy2(ROOT / "benchmark/realworld/remaining" / name, evaluator / name)

prior_index = read(BASE / "v29-adjudications.json")
prior = {(row["pair"], event["event_sha256"]): event
         for row in prior_index["observations"] for event in row["events"]}
observations = [row for row in read(BASE / "v31-pilot-observations.json")["observations"]
                if row["pair"] in selected]
observations.extend(read(BASE / "v31-reviewed-repetition2.json")["observations"])
targets = {target["pair"]: target for target in read(ROOT / "benchmark/realworld/followup/targets.json")["targets"]}
baselines = {"historical": read(ROOT / "benchmark/realworld/followup/baseline-observations.json"),
             "599855d": read(BASE / "baseline-observations.json")}
directories = {EDPB: "source-boundaries-baseline-edpb-review", DESIGN: "source-boundaries-design-v12-review",
               INCIDENT: "source-boundaries-incident-v15-review", SHA: "source-boundaries-sha-v9-review",
               SSDF: "source-boundaries-ssdf-v19-review",
               AUTH: "source-boundaries-auth-v29-review", BERT: "source-boundaries-baseline-bert-review"}
sources_cache = {}

# These rationales reflect page-image and native-source review. They are not
# inferred from the diff engine's own declaration that a range changed.
reasons = {
    (AUTH, 0): "The abstract replaces the opening statement about federal-agency digital-identity requirements with a focus on authentication, reorders the standards-scope qualification, and changes the superseded publication from corresponding sections of SP 800-63-2 to SP 800-63B. Both page images and raw source text support the changed paragraph. This whole-node range retains one old outer literal space and exceeds the fixed extent by one glyph.",
    (AUTH, 1): "The corresponding usability bullet changes consider form-factor constraints as they are to consider form-factor constraints, which are. This finite first-line comparison retains one trailing literal space per side. The surrounding out-of-band-authentication section and the next line support the local correspondence. It is outside the abstract target.",
    (AUTH, 2): "The raw source-cut parent retains the same 765 old and 641 new abstract glyphs as the original whole-node review, including the old trailing literal space. The old Keywords endpoint needs the explicit paint-order proof for its ascending roundoff step; no endpoint glyph enters the compared body.",
    (AUTH, 3): "The additional abstract view retains exactly the frozen 764 old and 641 new glyphs. It excludes only the one old mandatory outer literal space recorded in the retained raw parent, and preserves all interior words and source spaces. This is the 1405-atom fixed-target recovery, not a second abstract edit.",
    (AUTH, 4): "This raw parent retains the same usability-bullet source range and as they are/which are change as the original whole-node review. It preserves both trailing literal spaces and supports the finer cut certificate.",
    (AUTH, 5): "The finer usability-bullet view excludes only the two outer literal spaces, retaining the punctuation and wording change and every interior source. It is not another natural target recovery.",
    (EDPB, 11): "The page images show paragraphs 96, 97 and the beginning of 98 renumbered to 98, 99 and 100, and references 32/33 renumbered to 39/40. The intervening prose and Form of the contract or other legal act heading are unchanged. Number glyphs independently prove a source change; reconstructed gaps are not declared literal spaces. This is outside the fixed target.",
    (EDPB, 18): "Paragraph 162 becomes 165 and adds the internal-relationship qualification and the statement that a data subject may contact either joint controller; the following paragraph start changes 163 to 166. The new page visibly retains the duplicate period and GPDR spelling, which are not corrected by the comparison. The finite range and all interior source spacing remain as acquired. This is outside the fixed target.",
    (EDPB, 23): "The Determines table-of-contents entry changes its printed destination from 10 to 11, with different literal dot leaders and spacing. Both contents pages support the source change. It is not a prose-target recovery; the raw parent retains one new trailing literal space.",
    (EDPB, 24): "The finer Determines contents-entry view retains the page-number and literal leader changes while excluding only the one new outer space. The raw parent stays reported. This is a non-prose reference change.",
    (SSDF, 4): "The workforce-category list retains its Systems Requirements Planning through Management wording while removing old line numbers 147 through 155. The interval starts after the corresponding first Securely Provision line and ends before the Analyze item. The page images and raw paint order support numbering presence, not a changed workforce list. One outer literal space per side stays in this parent.",
    (SSDF, 5): "The finer workforce-list view excludes only one outer literal space per side, retaining every old line-number glyph and all interior list content. The raw parent remains reported; this is outside the abstract target and does not add a natural prose recovery.",
    (BERT, 11): "The corresponding GLUE-description line changes summarized in Wang et al. (2018): to summarized in Wang et al. (2018a):. The section moves into the new appendix, but both page images support this local source correspondence and citation suffix change. It is outside the abstract target and is not another recovered natural pair.",
}
for index, before, after in [(10,83,85),(12,118,121),(13,123,126),(14,137,140),
                            (15,138,141),(16,144,148),(17,158,161),(19,178,180),
                            (20,179,181),(21,185,187),(22,189,191)]:
    reasons[EDPB, index] = (f"The corresponding first line changes paragraph label {before} to {after}. "
        "The page images show unchanged prose within this finite line; its number glyphs independently prove a source difference. "
        "Raw literal spaces and reconstructed gaps retain their distinct provenance. This numbering output is outside the fixed prose target.")

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
            record["identity_revalidation"] = "The frozen v31 event/source digest equals the source-reviewed v29 digest."
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
                paint_evidence = None
                if review.get("source_cuts") and review["source_cuts"]["population"].get("row_order"):
                    paint_evidence = check_paint_population(review, side, sources[side], report["comparison"]["scopes"][0]["result"]["matching"]["scope"][side])
                pages = sorted({glyph["page"] for glyph in glyphs_in_range})
                evidence["sides"][side] = {"pages": pages, "raw_text": raw, "displayed_text": displayed,
                    "raw_source_spaces": raw.count(" "), "exact_raw_text": raw == displayed,
                    "paint_population": paint_evidence,
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

save(BASE / "v31-adjudications.json", {"version": 1,
    "scope": "All B outputs for six selected target pairs in two observations, plus the design-default miss in one observation. Raw parents and refined views are not distinct author edits or natural pairs.",
    "inherited_adjudications": ref(BASE / "v29-adjudications.json"), "observations": adjudications})
save(BASE / "v31-target-scores.json", {"version": 1, "evaluator": ref(evaluator / "verify.py"),
    "adjudications": ref(BASE / "v31-adjudications.json"),
    "target_references": {pair: targets[pair]["references"] for pair in sorted(selected)}, "scores": scores,
    "status": "Six reviewed natural pairs from three producers score B against the surviving historical baseline. EDPB is retained 599855d capability; BERT, NIST SHA, NIST incident handling, NIST SSDF and NIST authentication are additional relative to that baseline. The design-default target remains missed. The numerical six-pair threshold is met in these selected repeated reviews. Full-panel repeated development evidence, natural segmented recovery and new unseen gates remain outstanding."})
print(json.dumps(scores, indent=2))
