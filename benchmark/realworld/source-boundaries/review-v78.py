"""Check new order-independent witnesses against retained native evidence."""

import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
PHASE = ROOT / "benchmark/realworld/source-boundaries"
CACHE = ROOT / "benchmark/realworld/cache/source-boundaries-order-proof-v78"
spec = importlib.util.spec_from_file_location("prior_review", PHASE / "review-v75.py")
prior_review = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prior_review)
read, ref, verify = prior_review.read, prior_review.ref, prior_review.verify


def review_pair(pair, expected_pages, expected_tokens):
    directory = CACHE / "source-exports" / pair
    current = next(row for row in read(PHASE / "v78-panel-observations.json")["observations"]
                   if row["pair"] == pair)
    previous = next(row for row in read(PHASE / "v75-panel-observations.json")["observations"]
                    if row["pair"] == pair)
    report = verify.read_reference(current["report"])
    source = read(directory / "review/sources.json")
    events = verify.events(report)
    assert [(verify.event_digest(e), e["review"]) for e in events] == [
        (verify.event_digest(e), e["review"])
        for e in verify.events(read(directory / "report.json"))]
    before = verify.events(verify.read_reference(previous["report"]))
    before_digests = {verify.event_digest(event) for event in before}
    target = next(row for row in read(ROOT / "benchmark/realworld/followup/targets.json")["targets"]
                  if row["pair"] == pair)
    records = []
    for event in events:
        digest = verify.event_digest(event)
        if digest in before_digests:
            continue
        review = event["review"]
        assert event["category"] == "B" and review.get("source_cuts") is None
        assert review["comparison"].get("text_mask") is None
        proof = review["comparison"]["text_change_proof"]
        assert proof["token"]["Scalar"] in expected_tokens
        old_review = next(e["review"] for e in before
                          if e["review"]["old_sources"] == review["old_sources"]
                          and e["review"]["new_sources"] == review["new_sources"])
        assert {k: v for k, v in old_review.items() if k != "comparison"} == {
            k: v for k, v in review.items() if k != "comparison"}
        root = report["comparison"]["scopes"][0]["result"]["matching"]["scope"]
        checks = {}
        pages = []
        for side in ("old", "new"):
            assert source[side]["summary"]["revision"] == report[side]["revision"]
            checks[side] = prior_review.native.side_check(
                review, side, source[side], root[side],
                prior_review.native.validators()["check_original_cuts"])
            assert checks[side]["raw"].count(proof["token"]["Scalar"]) == proof[side + "_required"]
            assert proof[side + "_required"] == proof[side + "_possible"]
            page = checks[side]["page"]
            assert page == expected_pages[side]
            image = directory / "review" / f"{side}-region-{page}.png"
            if pair == "edpb-restrictions-v1-to-final":
                inspected = (ROOT / "benchmark/realworld/cache/source-boundaries-closed-components-v75"
                             / "source-exports" / pair / "review" / image.name)
                assert ref(image)["sha256"] == ref(inspected)["sha256"]
            pages.append(ref(image))
        assert proof["old_required"] != proof["new_required"]
        rationale = (
            "The inspected page images show footnote markers 9→10, 10→11 and 11→12 "
            "in the three respective ranges; the first range also gains a period. "
            "The original glyphs independently reproduce the reported period or digit "
            "multiplicity. This proves bounded source-content change under every ordering, "
            "without asserting an exact mask, a lexical policy change or a fixed-target gain."
            if pair == "edpb-restrictions-v1-to-final" else
            "The inspected SHA-1 preprocessing list changes from three steps to two: "
            "initialization comes first and padding/parsing now refer to Section 5. "
            "The original superscript notation contains four left parentheses before "
            "and one after. These raw glyph counts prove bounded source-text change "
            "without an ordered mask or a claim about algorithmic equivalence."
        )
        row = {
            "pointer": event["pointer"], "event_sha256": digest,
            "verdict": "source_supported",
            "source_content_rationale": rationale,
            "correspondence_rationale": (
                "The complete finite interval and closure fields match the earlier reviewed range. "
                "Native glyph census, node bands, source conflicts, inventory and paint closure "
                "are independently rechecked against the fresh export."),
            "source_evidence": [ref(directory / "review/sources.json"),
                                ref(directory / "review/manifest.json"),
                                ref(directory / "capture.json"),
                                target["references"]["annotation"],
                                target["references"]["resolution"],
                                ref(PHASE / "review-v31.py"), ref(PHASE / "review-v51.py"),
                                ref(PHASE / "review-v67.py"), ref(PHASE / "review-v75.py"),
                                ref(Path(__file__)), *pages],
        }
        records.append({"pair": pair, "report": current["report"], "event": row,
                        "source_checks": checks, "count_witness": proof, "reviewed_pages": pages})
    return records


def main():
    edpb = review_pair("edpb-restrictions-v1-to-final", {"old": 7, "new": 8}, (".", "0", "1"))
    nist = review_pair("nist-sha-1803-to-1804", {"old": 22, "new": 22}, ("(",))
    assert len(edpb) == 3 and len(nist) == 1
    records = edpb + nist
    result = {"version": 78, "reproducer": ref(Path(__file__)),
              "scope": "Four conservative replacements of ordered masks, independently checked against raw glyph counts and four inspected original page images.",
              "records": records}
    (PHASE / "v78-added-source-reviews.json").write_text(json.dumps(result, indent=2) + "\n")
    print("Reviewed four order-independent witnesses.")


if __name__ == "__main__":
    main()
