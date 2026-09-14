"""Review component discovery against native sources and independently solved masks."""

from collections import Counter
import importlib.util
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
BASE = ROOT / "benchmark/realworld/source-boundaries"
CACHE = ROOT / "benchmark/realworld/cache/source-boundaries-closed-components-v75"


def load(name):
    path = BASE / f"review-v{name}.py"
    spec = importlib.util.spec_from_file_location(f"review{name}", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


native = load(67)
masks = load(51)
read, ref, verify = native.read, native.ref, native.verify


def whole_view(review, side, source):
    nodes = {node["id"]: node for node in source["graph"]["nodes"]}
    members = review["comparison"][side]
    assert [atom for member in members for atom in nodes[member]["sources"]] == review[side + "_sources"]
    result = {key: [] for key in ("tokens", "origins", "backed", "optional")}
    for member in members:
        view = nodes[member]["content"]["view"]
        assert view["normalization"] == {"kind": "exact"}
        for token, origins, backed in zip(view["tokens"], view["origins"], view["source_backed"], strict=True):
            assert set(token) == {"Scalar"} and len(token["Scalar"]) == 1
            scalar = token["Scalar"]
            result["tokens"].append(scalar)
            result["origins"].append(origins)
            result["backed"].append(backed)
            result["optional"].append(not backed and scalar in " \t\n\r\v\f")
    assert "".join(result["tokens"]) == review["comparison"]["operation"][side]
    return result


def review_pair(pair, current, prior, diagnostic=False):
    directory = CACHE / "source-exports" / pair
    target_path = BASE / "unseen-v8/targets-corrected.json" if diagnostic else ROOT / "benchmark/realworld/followup/targets.json"
    target = next(t for t in read(target_path)["targets"] if t["pair"] == pair)
    source = read(directory / "review/sources.json")
    report = verify.read_reference(current)
    exported = read(directory / "report.json")
    events = verify.events(report)
    assert [(verify.event_digest(e), e.get("review")) for e in events] == [
        (verify.event_digest(e), e.get("review")) for e in verify.events(exported)]
    before = {verify.event_digest(e) for e in verify.events(verify.read_reference(prior))}
    root = report["comparison"]["scopes"][0]["result"]["matching"]["scope"]
    records = []
    for event in events:
        digest = verify.event_digest(event)
        if digest in before:
            continue
        assert event["category"] == "B"
        review = event["review"]
        for side in ("old", "new"):
            assert report[side]["revision"] == source[side]["summary"]["revision"]
        checks = {side: native.side_check(review, side, source[side], root[side],
                                         native.validators()["check_original_cuts"])
                  for side in ("old", "new")}
        proof = review["comparison"].get("text_change_proof")
        independent_mask = None
        if proof:
            token = proof["token"]["Scalar"]
            for side in ("old", "new"):
                assert Counter(checks[side]["raw"])[token] == proof[side + "_required"] == proof[side + "_possible"]
            assert proof["old_required"] != proof["new_required"]
        elif not diagnostic:
            project = masks.expanded_view if review.get("source_cuts") else whole_view
            views = {side: project(review, side, source[side]) for side in ("old", "new")}
            independent_mask = masks.check_mask(review, views)
        pages = []
        for side, check in checks.items():
            rendering = next(r for r in source[side]["summary"]["rendered_sources"] if r["page"] == check["page"])
            pages.append(ref(directory / "review" / f"{side}-region-{rendering['id']}.png"))
        old, new = checks["old"]["display"], checks["new"]["display"]
        if diagnostic:
            assert old.replace("that recovery planning", "recovery planning").replace("the information systems", "information systems") == new
            rationale = ("The Abstract loses 'that' before 'recovery planning' and 'the' before 'information systems'. "
                         "Both inspected pages bound the full body by Abstract and Keywords. Old margin counters and the new vertical watermark lie outside the proved source band. "
                         "The whole interval, raw cut parent and outer-space refinement are three non-owning views of the same two word deletions. This retired series is not unseen evidence.")
        elif old.replace(" ", "") == new.replace(" ", ""):
            assert independent_mask
            for side in ("old", "new"):
                glyphs = {g["id"]: g for g in source[side]["native"]["items"]}
                for claim in review["comparison"]["text_mask"][side]:
                    assert all(glyphs[atom["glyph"]]["text"] == {"Mapped": " "} for atom in claim["sources"])
            rationale = ("The inspected bullet wording is visibly unchanged. Only the retained literal-source spacing differs, including bold-run joins and trailing spaces. "
                         "Independent all-optimal alignment over every optional separator interpretation reproduces the complete reported mask and source-cost bounds. "
                         "This is a source-space encoding difference, not a lexical change, author-intent claim or fixed prose-target gain.")
        elif old.startswith(("31.", "32.")):
            rationale = ("The first body line is visibly renumbered from 31 to 32 or from 32 to 33; its prose is unchanged. "
                         "Both original-token cut projections and all-optimal masks were checked independently across every optional spacing interpretation. "
                         "The raw and outer-space-refined views do not count as separate author changes or prose-target gains.")
        else:
            assert old.startswith("33.") and new.startswith("34.") and proof
            rationale = ("The renumbered example expands investigation into administrative inquiry, disciplinary proceedings and workplace harassment, adding privacy and access-right qualifications. "
                         "The inspected pages and original glyphs support the finite range; one versus two commas independently proves change. Exact masks remain unresolved. "
                         "The raw and refined views retain the same lexical change and are outside the fixed target.")
        row = {"pointer": event["pointer"], "event_sha256": digest, "verdict": "source_supported",
               "source_content_rationale": rationale,
               "correspondence_rationale": "Accepted native endpoints delimit the conditional finite interval. Original cuts, exact glyph census, descending native node bands, inventory, source conflicts and disjoint paint are independently checked; graph grouping is not a semantic paragraph claim.",
               "source_evidence": [ref(directory / "review/sources.json"), ref(directory / "review/manifest.json"),
                                   target["references"]["annotation"], target["references"]["resolution"],
                                   ref(directory / "capture.json"), ref(BASE / "review-v31.py"),
                                   ref(BASE / "review-v51.py"), ref(BASE / "review-v67.py"), ref(Path(__file__)), *pages]}
        records.append({"pair": pair, "report": current, "event": row, "source_checks": checks,
                        "independent_mask": independent_mask, "reviewed_pages": pages})
    return records


def main():
    import json
    current = read(BASE / "v75-panel-observations.json")
    prior = {r["pair"]: r for r in read(BASE / "v67-panel-observations.json")["observations"]}
    pair = "edpb-restrictions-v1-to-final"
    row = next(r for r in current["observations"] if r["pair"] == pair)
    records = review_pair(pair, row["report"], prior[pair]["report"])
    assert len(records) == 8 and sum(r["independent_mask"] is not None for r in records) == 6
    result = {"version": 75, "build": current["build"], "reproducer": ref(Path(__file__)),
              "scope": "Eight additional finite B views checked against five inspected EDPB page images, original source populations, six independently solved masks and two comma-multiplicity proofs. No new fixed target recovery is claimed.",
              "records": records}
    (BASE / "v75-added-source-reviews.json").write_text(json.dumps(result, indent=2) + "\n")
    pair = "nist-184-draft-to-final"
    directory = CACHE / "source-exports" / pair
    previous = ROOT / "benchmark/realworld/cache/source-boundaries-unseen-v8/current-repetition1" / f"{pair}-text.json"
    records = review_pair(pair, ref(directory / "report.json"), ref(previous), diagnostic=True)
    assert len(records) == 3
    target = next(t for t in read(BASE / "unseen-v8/targets-corrected.json")["targets"] if t["pair"] == pair)
    core, extent, _ = verify.historical.target_sources(target)
    events = verify.events(read(directory / "report.json"))
    hits = [verify.event_digest(e) for e in events if e["category"] == "B" and verify.range_recovery(e["review"], core, extent)["source_range_hit"]]
    assert len(hits) == 1 and hits[0] in {r["event"]["event_sha256"] for r in records}
    result = {"version": 75, "diagnostic_only": True, "build": current["build"],
              "target": ref(BASE / "unseen-v8/targets-corrected.json"), "exact_range_hits": hits,
              "scope": "Three new finite B views are source-reviewed on a retired series; the other ten outputs are not newly adjudicated here. One view matches the unchanged 1,899-atom target. No fresh unseen recovery or exact-mask validation is claimed by this diagnostic.", "records": records}
    (BASE / "unseen-v8-diagnosis/v75-nist-184-source-review.json").write_text(json.dumps(result, indent=2) + "\n")
    print("Reviewed eight development additions and three retired-series diagnostic views.")


if __name__ == "__main__":
    main()
