"""Bind source-reviewed event identities to both frozen full-panel repetitions."""

from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
BASE = ROOT / "benchmark/realworld/source-boundaries"
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
    path = BASE / name
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def references(value):
    if isinstance(value, dict):
        if set(value) == {"path", "sha256"}:
            yield value
        else:
            for child in value.values():
                yield from references(child)
    elif isinstance(value, list):
        for child in value:
            yield from references(child)


registration, registered = verify.registration()
baseline = registered["baseline-observations"]
baseline_rows = {(row["pair"], row["repetition"]): row for row in baseline["observations"]}
targets = {row["pair"]: row for row in registered["targets"]["targets"]}
indices = [read(BASE / name) for name in (
    "v33-panel-observations.json", "v33-panel-repetition2-observations.json")]
observations = [row for index in indices for row in index["observations"]]
binary = verify.read_reference(indices[0]["build"])["binary"]
verify.observations({"observations": observations}, registered["panel"], binary)
assert indices[0]["build"] == indices[1]["build"]
prior_reference = ref(BASE / "v31-adjudications.json")
prior = {(row["pair"], event["event_sha256"]): event
         for row in read(BASE / "v31-adjudications.json")["observations"] for event in row["events"]}
checks_reference = ref(BASE / "v33-extra-source-checks.json")
checks = read(BASE / "v33-extra-source-checks.json")
assert len(checks) == 137 and not any(row["errors"] for row in checks)
checked = {(row["pair"], row["digest"]): row for row in checks}
assert len(checked) == len(checks)
visual_reference = ref(BASE / "v33-extra-page-review.json")
visual = read(BASE / "v33-extra-page-review.json")
assert visual["observations"] == ref(BASE / "v33-panel-observations.json")
verified_references = set()


def checked_references(values):
    result = {}
    for reference in values:
        identity = reference["path"], reference["sha256"]
        if identity not in verified_references:
            verify.historical.checked_path(reference)
            verified_references.add(identity)
        result[identity] = reference
    return list(result.values())


full, formal, summaries, identities = [], [], [], {}
for observation in observations:
    pair, repetition = observation["pair"], observation["repetition"]
    report = verify.read_reference(observation["report"])
    events = verify.events(report)
    signatures = [verify.event_digest(event) for event in events]
    if pair in identities:
        assert identities[pair] == signatures
    else:
        identities[pair] = signatures
    rows = []
    for event, digest in zip(events, signatures):
        identity = pair, digest
        if identity in prior:
            row = copy.deepcopy(prior[identity])
            evidence = list(references(row["source_evidence"])) + [prior_reference]
            row["inherited_source_review"] = row.pop("source_evidence")
            row["identity_revalidation"] = "The complete event/source digest equals the source-reviewed V31 identity."
        else:
            proof = checked[identity]
            assert proof["pointer"] == event["pointer"] and proof["category"] == event["category"]
            page_review = visual["pairs"][pair]
            assert proof["source_path"] == page_review["sources"]["path"]
            for side in ("old", "new"):
                assert proof["sides"][side]["display"] == event["operation"][side]
                pages = proof["sides"][side]["pages"]
                assert all(page in page_review["viewed_pages"][side] for page in pages)
            if event["category"] == "A":
                assert {tuple(atom) for atom in proof["strict_source_proof"]["changed_sources"]} == event["sources"]
            rationale = page_review["reasons"][event["pointer"].rsplit("/", 1)[1]]
            row = {"event_sha256": digest, "verdict": "source_supported",
                   "source_content_rationale": rationale, "source_checks": proof,
                   "correspondence_rationale": (
                       "The original exact footer views independently admit only the two claimed changed source tokens."
                       if event["category"] == "A" else
                       "The accepted finite interval and original source cuts retain the corresponding page passage. "
                       "Independent glyph projection and any paint-row census are recorded. "
                       "Unresolved spacing does not become an exact literal-space or strict-mask claim.")}
            evidence = list(references(page_review)) + [visual_reference, checks_reference,
                        ref(BASE / "review-v33.py"), ref(BASE / "review-v31.py")]
        row["pointer"] = event["pointer"]
        row["source_evidence"] = checked_references(evidence + list(targets[pair]["references"].values()))
        rows.append(row)
    full.append({**observation, "events": rows})
    previous = baseline_rows[pair, repetition]
    before = verify.read_reference(previous["report"]) if previous.get("report") else None
    counts = Counter(verify.event_digest(event) for event in verify.events(before))
    needed = []
    for event, row in zip(events, rows):
        digest = verify.event_digest(event)
        if counts[digest]:
            counts[digest] -= 1
        elif event["category"] != "C":
            needed.append(row)
    formal.append({**observation, "events": needed})
    target = targets[pair]
    core, extent, controls = verify.historical.target_sources(target)
    gold = None
    if target["strict_event_gold"] is not None:
        gold = {"operation": target["strict_event_gold"],
                "sources": {tuple(atom) for atom in target["strict_changed_position_gold"]}}
    score = verify.pair_recovery(before, report, core, extent, controls, needed, gold)
    assert score["correct"], (pair, repetition, score)
    summaries.append({"pair": pair, "repetition": repetition, "score": score})

assert sum(map(len, identities.values())) == 222
save("v33-repeated-observations.json", {"version": 1, "repetitions": 2,
     "build": indices[0]["build"], "observations": observations})
save("v33-adjudications.json", {"version": 1, "scope": "All A/B outputs in both complete frozen panels.",
     "inherited_adjudications": prior_reference, "source_checks": checks_reference,
     "page_review": visual_reference, "observations": full})
save("v33-formal-adjudications.json", {"version": 1,
     "scope": "Only events additional to each corresponding exact historical baseline attempt.",
     "all_outputs": ref(BASE / "v33-adjudications.json"), "observations": formal})
save("v33-reviewed-panel-scores.json", {"version": 1, "scope": "Repeated source review and fixed-target scores; controls and unseen gates are separate.",
     "baseline": ref(BASE / "baseline-reacquisition.json"),
     "observations": ref(BASE / "v33-repeated-observations.json"),
     "adjudications": ref(BASE / "v33-formal-adjudications.json"), "scores": summaries})
print("Verified 222 distinct source-supported events in two independent 36-pair panels.")
